//! Inventory Manager Implementation
use std::sync::Arc;

use lampo_common::error;
use lampo_common::model::response::NetworkInfo;
use lampo_common::model::GetInfo;

use crate::async_run;

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

    #[deprecated]
    pub async fn get_info_node(&self) -> error::Result<GetInfo> {
        let chain = self.channel_manager.conf.network.to_string();
        let alias = self.channel_manager.conf.alias.clone();
        // we have to put "" in case of alias missing as cln provide us with a random alias.
        let alias = alias.unwrap_or_default();
        let (block_hash, height) = self
            .channel_manager
            .onchain
            .backend
            .get_best_block()
            .await
            .unwrap();
        let blockheight = height.unwrap_or_default();
        let lampo_dir = self.channel_manager.conf.root_path.to_string();
        // We provide a vector here as there may be other types of address in future like tor and ipv6.
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

        let wallet_tips = self.channel_manager.wallet_manager().wallet_tips().await?;
        let getinfo = GetInfo {
            node_id: self.channel_manager.manager().get_our_node_id().to_string(),
            peers: self.peer_manager.manager().list_peers().len(),
            channels: self.channel_manager.manager().list_channels().len(),
            chain,
            alias,
            color: "#000000".to_string(),
            blockheight,
            lampo_dir,
            address: address_vec,
            block_hash: block_hash.to_string(),
            wallet_height: wallet_tips.to_consensus_u32() as u64,
        };
        Ok(getinfo)
    }
}
