use std::sync::Arc;

use async_trait::async_trait;

use crate::bitcoin::absolute::Height;
use crate::bitcoin::{Amount, FeeRate};
use crate::bitcoin::{ScriptBuf, Transaction};
use crate::conf::LampoConf;
use crate::error;
use crate::keys::LampoKeys;
use crate::model::response::{NewAddress, Utxo};

/// Snapshot of the wallet's chain-sync progress, surfaced via `getinfo`.
#[derive(Debug, Clone)]
pub struct WalletSyncStatus {
    /// Height of the most recently scanned block.
    pub scan_height: u32,
    /// Whether a chain sync is currently running.
    pub in_progress: bool,
}

impl WalletSyncStatus {
    /// Scan progress toward `chain_tip`, clamped to 0-100. Returns 100 once the
    /// scan has caught up (or when there is no chain to scan yet).
    pub fn progress_percent(&self, chain_tip: u32) -> u8 {
        if chain_tip == 0 || self.scan_height >= chain_tip {
            return 100;
        }
        ((self.scan_height as u64 * 100) / chain_tip as u64) as u8
    }
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

    /// Return a snapshot of the wallet sync progress (scan height and whether a
    /// sync is currently running).
    async fn sync_status(&self) -> error::Result<WalletSyncStatus>;

    /// Sync the wallet.
    async fn sync(&self) -> error::Result<()>;

    /// Run a task for wallet sync operation, this usually need to
    /// be run in a `tokio::spawn(wallet.listen())`.
    async fn listen(self: Arc<Self>) -> error::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::WalletSyncStatus;

    #[test]
    fn progress_percent_computes_and_clamps() {
        let status = |scan_height| WalletSyncStatus {
            scan_height,
            in_progress: true,
        };
        // Mid-scan reports a proportional percentage.
        assert_eq!(status(50).progress_percent(100), 50);
        // Caught up to (or past) the tip reports 100, never overflows.
        assert_eq!(status(100).progress_percent(100), 100);
        assert_eq!(status(150).progress_percent(100), 100);
        // No chain to scan yet is treated as fully synced.
        assert_eq!(status(0).progress_percent(0), 100);
    }
}
