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
}

#[derive(Debug, Deserialize, Serialize, Apiv2Schema)]
pub struct NetworkInfo {
    pub address: String,
    pub port: u64,
}
