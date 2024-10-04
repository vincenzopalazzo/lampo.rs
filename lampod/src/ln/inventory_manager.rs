//! Inventory Manager Implementation
use std::sync::Arc;

use lampo_common::error;
use lampo_common::model::response::NetworkInfo;
use lampo_common::model::GetInfo;

use super::{LampoChannelManager, LampoPeerManager};

pub struct LampoInventoryManager {
    peer_manager: Arc<LampoPeerManager>,
    channel_manager: Arc<LampoChannelManager>,
}

impl LampoInventoryManager {
    pub fn new(
        peer_manager: Arc<LampoPeerManager>,
        channel_manager: Arc<LampoChannelManager>,
    ) -> Self {
        Self {
            peer_manager,
            channel_manager,
        }
    }
}
