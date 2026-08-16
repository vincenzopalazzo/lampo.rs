use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::bitcoin::absolute::Height;
use crate::bitcoin::{Amount, FeeRate};
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
    /// Returns `(wallet, is_new)` where `is_new` indicates whether a
    /// fresh wallet was created.
    async fn make_or_restore(conf: Arc<LampoConf>) -> error::Result<(Self, bool)>
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
            Ok((wallet, false))
        } else {
            std::fs::create_dir_all(conf.path())
                .map_err(|e| error::anyhow!("Failed to create wallet directory: {e}"))?;
            let (wallet, mnemonic) = Self::new(conf).await?;
            // FIXME: we should give the possibility to encrypt this file.
            std::fs::write(&words_path, &mnemonic)
                .map_err(|e| error::anyhow!("Failed to write wallet.dat: {e}"))?;
            Ok((wallet, true))
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
}
