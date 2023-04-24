//! Inventory Manager Implementation
use std::sync::Arc;

use lampo_common::error;
use lampo_common::json;

use super::{LampoChannelManager, LampoPeerManager};
use crate::actions::InventoryHandler;
use crate::events;

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

impl InventoryHandler for LampoInventoryManager {
    fn handle(&self, event: events::InventoryEvent) -> error::Result<()> {
        use events::InventoryEvent;
        use lampo_common::model::GetInfo;

        match event {
            InventoryEvent::GetNodeInfo(chan) => {
                let getinfo = GetInfo {
                    node_id: self.channel_manager.manager().get_our_node_id().to_string(),
                    peers: self.peer_manager.manager().get_peer_node_ids().len(),
                    channels: 0,
                };
                let getinfo = json::to_value(getinfo)?;
                chan.send(getinfo)?;
                Ok(())
            }
        }
    }
}
