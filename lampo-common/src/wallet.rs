use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::bitcoin::absolute::Height;
use crate::bitcoin::psbt::Psbt;
use crate::bitcoin::{Amount, FeeRate, OutPoint, TxOut, Txid};
use crate::bitcoin::{Block, BlockHash, ScriptBuf, Transaction};
use crate::chainsync::ChainSyncCoordinator;
use crate::conf::LampoConf;
use crate::error;
use crate::keys::LampoKeys;
use crate::model::response::{NewAddress, Utxo};

/// A lightweight reference to a block (height + hash). Pure `bitcoin` types,
/// so a chain backend and the wallet can exchange chain positions without the
/// wallet depending on LDK or the backend depending on BDK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockRef {
    pub height: u32,
    pub hash: BlockHash,
}

/// Wallet manager trait that define a generic interface
/// over Wallet implementation!
#[async_trait]
pub trait WalletManager: Send + Sync {
    /// Generate a new wallet for the network
    async fn new(conf: Arc<LampoConf>) -> error::Result<(Self, String)>
    where
        Self: Sized;

    /// Restore a previous created wallet from a network and a mnemonic_words
    async fn restore(network: Arc<LampoConf>, mnemonic_words: &str) -> error::Result<Self>
    where
        Self: Sized;

    /// Create or restore a wallet from persisted state.
    ///
    /// If a `wallet.dat` file exists in the config directory, the wallet
    /// is restored from the mnemonic stored there. Otherwise, a new wallet
    /// is created and the mnemonic is persisted to `wallet.dat`.
    ///
    /// Returns `(wallet, is_new, mnemonic)`. `mnemonic` is `Some` only when
    /// a fresh wallet was created, so callers that must show the seed (the
    /// `new-wallet` command) do not re-read the persistence file and stay
    /// correct if an implementation overrides this method.
    async fn make_or_restore(conf: Arc<LampoConf>) -> error::Result<(Self, bool, Option<String>)>
    where
        Self: Sized,
    {
        let words_path = format!("{}/wallet.dat", conf.path());
        if Path::new(&words_path).exists() {
            let mnemonic = std::fs::read_to_string(&words_path)
                .map_err(|e| error::anyhow!("Failed to read wallet.dat: {e}"))?;
            let mnemonic = mnemonic.trim().to_string();
            if mnemonic.is_empty() {
                return Err(error::anyhow!(
                    "wallet.dat exists but is empty at `{words_path}`. \
                     Please restore the mnemonic or remove the file to create a new wallet."
                ));
            }
            let wallet = Self::restore(conf, &mnemonic).await?;
            Ok((wallet, false, None))
        } else {
            std::fs::create_dir_all(conf.path())
                .map_err(|e| error::anyhow!("Failed to create wallet directory: {e}"))?;
            let (wallet, mnemonic) = Self::new(conf).await?;
            // SECURITY: owner-only, atomic write. A crash mid-write must not
            // leave a truncated seed, and the default umask must not leave
            // the mnemonic world-readable.
            write_mnemonic_file(&words_path, &mnemonic)?;
            Ok((wallet, true, Some(mnemonic)))
        }
    }

    /// Return the keys for ldk.
    fn ldk_keys(&self) -> Arc<LampoKeys>;

    /// return an on chain address
    async fn get_onchain_address(&self) -> error::Result<NewAddress>;

    /// Get the current balance of the wallet.
    async fn get_onchain_balance(&self) -> error::Result<u64>;

    /// Create the transaction from a script and return the transaction
    /// to propagate to the network.
    async fn create_transaction(
        &self,
        script: ScriptBuf,
        amount_sat: Amount,
        fee_rate: FeeRate,
        best_block: Height,
    ) -> error::Result<Transaction>;

    /// Return the list of transaction stored inside the wallet
    async fn list_transactions(&self) -> error::Result<Vec<Utxo>>;

    /// Return the last block height of the wallet, but we can abstract
    /// in the future the wallet tips info that we will need.
    async fn wallet_tips(&self) -> error::Result<Height>;

    /// The wallet's current best (checkpoint) block: height + hash. Lets a
    /// chain backend compute where to start syncing the wallet from in a
    /// unified sync pass.
    ///
    /// Synchronous so an LDK `Listen` adapter can call it without an async
    /// runtime; the underlying access is a short, non-blocking lookup.
    fn current_best_block(&self) -> error::Result<BlockRef>;

    /// Apply a connected block, advancing the wallet's view of the chain.
    /// Takes pure `bitcoin` types so the wallet never depends on LDK
    /// chain-sync; a backend drives this during unified sync.
    ///
    /// Synchronous so it can be driven directly from `Listen::block_connected`
    /// (which is itself sync) on any runtime flavor, keeping `LampoDaemon`
    /// embeddable. The critical section is a short BDK apply + persist.
    fn apply_block(&self, block: &Block, height: u32) -> error::Result<()>;

    /// Inject the chain-sync coordinator so the wallet can gate its scan on
    /// the LDK listener sync and report scan progress. Default no-op; the
    /// gate stays inactive until a coordinator is set. Pure lampo-common type
    /// (no LDK), keeping the wallet replaceable.
    fn set_coordinator(&self, _: Arc<ChainSyncCoordinator>) {}

    /// Sync the wallet.
    async fn sync(&self) -> error::Result<()>;

    /// Run a task for wallet sync operation, this usually need to
    /// be run in a `tokio::spawn(wallet.listen())`.
    async fn listen(self: Arc<Self>) -> error::Result<()>;

    /// Next wallet-owned script. Used for LDK destination, shutdown, and
    /// change so closed-channel funds land in this wallet.
    fn next_wallet_script(&self) -> error::Result<ScriptBuf> {
        error::bail!("wallet does not provide destination scripts")
    }

    /// Whether `script` is a revealed address of this wallet.
    fn is_mine(&self, _script: &ScriptBuf) -> bool {
        false
    }

    /// Confirmed, spendable UTXOs this wallet can sign (anchor CPFP).
    fn confirmed_utxos(&self) -> error::Result<Vec<(OutPoint, TxOut)>> {
        Ok(Vec::new())
    }

    /// Full previous transaction for `txid`, if the wallet has it.
    fn get_transaction(&self, _txid: Txid) -> error::Result<Option<Transaction>> {
        Ok(None)
    }

    /// Sign every input in `psbt` that this wallet controls.
    fn sign_psbt(&self, _psbt: Psbt) -> error::Result<Transaction> {
        error::bail!("wallet does not sign PSBTs")
    }
}

/// Persist a BIP39 mnemonic so a crash cannot truncate it and other local
/// users cannot read it.
///
/// The write goes to a sibling temp file created mode `0600`, then
/// `rename`d into place. `OpenOptions::mode` is masked by umask and only
/// applies on create, so permissions are tightened again after the write.
pub fn write_mnemonic_file(path: &str, mnemonic: &str) -> error::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let tmp_path = format!("{path}.tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&tmp_path)
        .map_err(|e| error::anyhow!("Failed to create {tmp_path}: {e}"))?;
    // FIXME: we should give the possibility to encrypt this file.
    file.write_all(mnemonic.as_bytes())
        .map_err(|e| error::anyhow!("Failed to write {tmp_path}: {e}"))?;
    file.sync_all()
        .map_err(|e| error::anyhow!("Failed to sync {tmp_path}: {e}"))?;
    drop(file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| error::anyhow!("Failed to set permissions on {tmp_path}: {e}"))?;
    }

    std::fs::rename(&tmp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        error::anyhow!("Failed to persist wallet.dat at `{path}`: {e}")
    })?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::write_mnemonic_file;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn mnemonic_file_is_owner_only_and_atomic() {
        let dir =
            std::env::temp_dir().join(format!("lampo-wallet-mnemonic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wallet.dat");
        let path_str = path.to_string_lossy().to_string();

        write_mnemonic_file(&path_str, "abandon abandon abandon").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        let body = std::fs::read_to_string(&path).unwrap();
        let tmp_left = dir.join("wallet.dat.tmp").exists();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(body, "abandon abandon abandon");
        assert!(!tmp_left, "temp file must be renamed into place");
        assert_eq!(
            mode & 0o777,
            0o600,
            "wallet.dat contains the mnemonic and must be 0600, got {:o}",
            mode & 0o777
        );
    }

    #[test]
    fn mnemonic_file_tightens_a_preexisting_loose_file() {
        let dir = std::env::temp_dir().join(format!(
            "lampo-wallet-mnemonic-loose-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wallet.dat");
        std::fs::write(&path, "old words").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        // Rewrite goes through a temp file, so a loose preexisting target
        // must not survive the rename.
        write_mnemonic_file(&path.to_string_lossy(), "abandon abandon abandon").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            mode & 0o777,
            0o600,
            "rewritten wallet.dat must be 0600, got {:o}",
            mode & 0o777
        );
    }
}
