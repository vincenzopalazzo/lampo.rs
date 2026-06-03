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
    pub wallet_height: u64,
    /// Live wallet scan height during initial catch-up, if known.
    pub wallet_scan_height: Option<u32>,
    /// Whether the LDK chain listeners have completed their initial sync.
    pub chain_listeners_synced: bool,
    /// Whether the full initial sync (listeners + wallet) has completed.
    pub initial_sync_complete: bool,
    /// Whether an initial sync is still in progress.
    pub sync_in_progress: bool,
}

#[derive(Debug, Deserialize, Serialize, Apiv2Schema)]
pub struct NetworkInfo {
    pub address: String,
    pub port: u64,
}
