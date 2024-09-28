//! Inventory Manager Implementation
use std::sync::Arc;

use lampo_common::model::response::NetworkInfo;
use lampo_common::{error, json};

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
                let alias = self.channel_manager.conf.alias.clone();
                // we have to put "" in case of alias missing as cln provide us with a random
                // alias.
                let alias = alias.unwrap_or_default();
                let (_, height) = self.channel_manager.onchain.backend.get_best_block()?;
                let blockheight = height.unwrap_or_default();
                let lampo_dir = self.channel_manager.conf.root_path.to_string();
                // We provide a vector here as there may be other types of address in future
                // like tor and ipv6.
                let mut address_vec = Vec::new();
                let address = self.channel_manager.conf.announce_addr.clone();
                if let Some(addr) = address {
                    let port = self.channel_manager.conf.port.clone();
                    // For now we don't iterate as there is only one type of address.
                    let address_info = NetworkInfo {
                        address: addr,
                        port,
                    };
                    address_vec.push(address_info);
                }
                let getinfo = GetInfo {
                    node_id: self.channel_manager.manager().get_our_node_id().to_string(),
                    peers: self.peer_manager.manager().list_peers().len(),
                    channels: self.channel_manager.manager().list_channels().len(),
                    chain,
                    alias,
                    blockheight,
                    lampo_dir,
                    address: address_vec,
                };
                let getinfo = json::to_value(getinfo)?;
                chan.send(getinfo)?;
                Ok(())
            }
        }
    }
}
