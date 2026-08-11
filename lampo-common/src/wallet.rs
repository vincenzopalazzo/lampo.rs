use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;

use crate::bitcoin::absolute::Height;
use crate::bitcoin::{Address, Network, OutPoint, Psbt, ScriptBuf, Transaction, TxOut, Txid};
use crate::bitcoin::{Amount, FeeRate};
use crate::conf::LampoConf;
use crate::error;
use crate::keys::LampoKeys;
use crate::ldk::sign::ChangeDestinationSource;
use crate::ldk::util::wallet_utils::{Utxo as LdkUtxo, WalletSource};
use crate::model::response::{NewAddress, Utxo};

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

    /// Sync the wallet.
    async fn sync(&self) -> error::Result<()>;

    /// Run a task for wallet sync operation, this usually need to
    /// be run in a `tokio::spawn(wallet.listen())`.
    async fn listen(self: Arc<Self>) -> error::Result<()>;

    /// Return all wallet UTXOs with at least one confirmation, available
    /// to fund fee bumps (anchor CPFP, HTLC transactions).
    async fn list_confirmed_utxos(&self) -> error::Result<Vec<(OutPoint, TxOut)>>;

    /// Return the full wallet transaction with the given txid, if known.
    async fn get_wallet_transaction(&self, txid: Txid) -> error::Result<Option<Transaction>>;

    /// Return a fresh change script from the wallet.
    async fn get_change_script(&self) -> error::Result<ScriptBuf>;

    /// Sign every wallet-owned input of the PSBT and return the resulting
    /// transaction. Inputs the wallet does not own are left untouched.
    async fn sign_psbt(&self, psbt: Psbt) -> error::Result<Transaction>;
}

/// [`ChangeDestinationSource`] backed by the node's on-chain wallet: swept
/// channel outputs land on a fresh wallet address.
pub struct LampoChangeDestination {
    wallet: Arc<dyn WalletManager>,
    network: Network,
}

impl LampoChangeDestination {
    pub fn new(wallet: Arc<dyn WalletManager>, network: Network) -> Self {
        Self { wallet, network }
    }
}

/// [`WalletSource`] backed by the node's on-chain wallet, used by LDK's
/// coin selection when funding anchor CPFP and HTLC fee bumps.
pub struct LampoWalletSource {
    wallet: Arc<dyn WalletManager>,
}

impl LampoWalletSource {
    pub fn new(wallet: Arc<dyn WalletManager>) -> Self {
        Self { wallet }
    }
}

impl WalletSource for LampoWalletSource {
    fn list_confirmed_utxos<'a>(
        &'a self,
    ) -> impl std::future::Future<Output = Result<Vec<LdkUtxo>, ()>> + Send + 'a {
        async move {
            let utxos = self.wallet.list_confirmed_utxos().await.map_err(|err| {
                log::error!(target: "lampo-wallet", "failed to list confirmed utxos: {err}");
            })?;
            // The wallet is BIP84, so every spendable output is P2WPKH;
            // skip anything else instead of misreporting its weight.
            let utxos = utxos
                .into_iter()
                .filter_map(|(outpoint, output)| {
                    if output.script_pubkey.is_p2wpkh() {
                        use crate::bitcoin::hashes::Hash;
                        let pubkey_hash = crate::bitcoin::WPubkeyHash::from_slice(
                            &output.script_pubkey.as_bytes()[2..22],
                        )
                        .ok()?;
                        Some(LdkUtxo::new_v0_p2wpkh(outpoint, output.value, &pubkey_hash))
                    } else {
                        log::warn!(
                            target: "lampo-wallet",
                            "skipping non-p2wpkh utxo `{outpoint}` for fee bumping"
                        );
                        None
                    }
                })
                .collect();
            Ok(utxos)
        }
    }

    fn get_prevtx<'a>(
        &'a self,
        outpoint: OutPoint,
    ) -> impl std::future::Future<Output = Result<Transaction, ()>> + Send + 'a {
        async move {
            self.wallet
                .get_wallet_transaction(outpoint.txid)
                .await
                .map_err(|err| {
                    log::error!(target: "lampo-wallet", "failed to load prev tx `{}`: {err}", outpoint.txid);
                })?
                .ok_or(())
        }
    }

    fn get_change_script<'a>(
        &'a self,
    ) -> impl std::future::Future<Output = Result<ScriptBuf, ()>> + Send + 'a {
        async move {
            self.wallet.get_change_script().await.map_err(|err| {
                log::error!(target: "lampo-wallet", "failed to derive a change script: {err}");
            })
        }
    }

    fn sign_psbt<'a>(
        &'a self,
        psbt: Psbt,
    ) -> impl std::future::Future<Output = Result<Transaction, ()>> + Send + 'a {
        async move {
            self.wallet.sign_psbt(psbt).await.map_err(|err| {
                log::error!(target: "lampo-wallet", "failed to sign fee-bump psbt: {err}");
            })
        }
    }
}

impl ChangeDestinationSource for LampoChangeDestination {
    fn get_change_destination_script<'a>(
        &'a self,
    ) -> impl std::future::Future<Output = Result<ScriptBuf, ()>> + Send + 'a {
        async move {
            let address = self.wallet.get_onchain_address().await.map_err(|err| {
                log::error!(target: "lampo-wallet", "failed to derive a sweep destination address: {err}");
            })?;
            let address = Address::from_str(&address.address).map_err(|err| {
                log::error!(target: "lampo-wallet", "invalid sweep destination address: {err}");
            })?;
            let address = address.require_network(self.network).map_err(|err| {
                log::error!(target: "lampo-wallet", "sweep destination address on wrong network: {err}");
            })?;
            Ok(address.script_pubkey())
        }
    }
}
