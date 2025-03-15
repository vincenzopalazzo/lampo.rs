use std::sync::Arc;

use async_trait::async_trait;

use crate::bitcoin::{ScriptBuf, Transaction};
use crate::conf::LampoConf;
use crate::error;
use crate::keys::LampoKeys;
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
        amount_sat: u64,
        fee_rate: u32,
    ) -> error::Result<Transaction>;

    /// Return the list of transaction stored inside the wallet
    async fn list_transactions(&self) -> error::Result<Vec<Utxo>>;

    /// Sync the wallet.
    async fn sync(&self) -> error::Result<()>;
}
