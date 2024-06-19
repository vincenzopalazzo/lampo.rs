use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct GetInfo {
    pub node_id: String,
    pub peers: usize,
    pub channels: usize,
    pub chain: String,
    pub alias: String,
    pub block_height: u32,
    pub best_height: u32,
    pub is_sycning: bool,
    pub lampo_dir: String,
    pub address: Vec<NetworkInfo>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NetworkInfo {
    pub address: String,
    pub port: u64,
}
