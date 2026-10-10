use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
pub struct GetInfo {
    pub node_id: String,
    pub peers: usize,
    pub channels: usize,
    pub chain: String,
    pub alias: String,
    pub blockheight: u32,
    pub lampo_dir: String,
    pub address: Vec<NetworkInfo>,
    pub block_hash: String,
    /// Persisted wallet checkpoint height.
    pub wallet_height: u64,
    /// Height the wallet has scanned up to (advances live during a sync).
    #[serde(default)]
    pub wallet_scan_height: u64,
    /// Whether lampo or its chain backend is still syncing.
    ///
    /// True during lampo's own listener/wallet catch-up, and also while the
    /// backend (bitcoind IBD) is still downloading blocks. See issue #111.
    #[serde(default)]
    pub sync_in_progress: bool,
    /// Wallet scan progress toward the chain tip, 0-100.
    ///
    /// Capped at 99 while the backend itself is still syncing, so a node
    /// caught up to an IBD tip does not advertise 100.
    #[serde(default)]
    pub sync_progress_percent: u8,
    /// The chain backend (bitcoind) is still downloading blocks. Distinct
    /// from lampo's own catch-up: both can be true, and either one means
    /// `sync_in_progress`.
    #[serde(default)]
    pub backend_syncing: bool,
}

#[derive(Debug, Deserialize, Serialize, Apiv2Schema)]
pub struct NetworkInfo {
    pub address: String,
    pub port: u64,
}
