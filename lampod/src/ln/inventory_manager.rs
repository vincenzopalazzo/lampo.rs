//! Inventory Manager Implementation
use std::sync::Arc;

use lampo_common::error;
use lampo_common::json;

use super::{LampoChannelManager, LampoPeerManager};
use crate::actions::InventoryHandler;
use crate::command;

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
    fn handle(&self, event: command::InventoryCommand) -> error::Result<()> {
        use command::InventoryCommand;
        use lampo_common::model::GetInfo;

        match event {
            InventoryCommand::GetNodeInfo(chan) => {
                let chain = self.channel_manager.conf.network.to_string();
                let getinfo = GetInfo {
                    node_id: self.channel_manager.manager().get_our_node_id().to_string(),
                    peers: self.peer_manager.manager().list_peers().len(),
                    channels: self.channel_manager.manager().list_channels().len(),
                    chain,
                };
                let getinfo = json::to_value(getinfo)?;
                chan.send(getinfo)?;
                Ok(())
            }
        }
    }
}
