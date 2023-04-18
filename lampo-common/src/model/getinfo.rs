use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct GetInfo {
    pub node_id: String,
    pub peers: usize,
    pub channels: usize,
}
