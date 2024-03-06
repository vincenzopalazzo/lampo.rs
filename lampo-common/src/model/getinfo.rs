use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct GetInfo {
    pub node_id: String,
    pub peers: usize,
    pub channels: usize,
    pub chain: String,
}
