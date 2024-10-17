//! Lampo handler implementation for LDK node
//!
//! Author: Vincenzo Palazzo <vincenzopalazzo@member.fsf.org>
use std::fmt;
use std::net::ToSocketAddrs;
use std::str::FromStr;
use std::sync::Arc;

use lampo_common::error::Ok;
use lampo_common::handler::ExternalHandler;
use lampo_common::model::request::NetworkInfo;
use lampo_common::model::response;
use ldk_node::bitcoin::Network;
use ldk_node::Builder;
use ldk_node::Node;

use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::json;

pub enum CommandSupported {
    GetInfo,
}

impl FromStr for CommandSupported {
    type Err = error::Error;

    fn from_str(s: &str) -> error::Result<Self> {
        match s {
            "getinfo" => Ok(CommandSupported::GetInfo),
            _ => error::bail!("The command `{s}` is not supported"),
        }
    }
}

impl fmt::Display for CommandSupported {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CommandSupported::GetInfo => write!(f, "getinfo"),
        }
    }
}

pub struct LDKNodeHandler {
    pub(crate) node: Node,
}

impl LDKNodeHandler {
    pub fn new(config: Arc<LampoConf>) -> Self {
        let mut node = Builder::new();
        node.set_network(config.network);
        // We should complete the configuration
        let esplora_url = match config.network {
            Network::Bitcoin => "https://blockstream.info/api",
            Network::Testnet => "https://blockstream.info/testnet/api",
            _ => unreachable!("The network `{}` is not supported", config.network),
        };
        node.set_esplora_server(esplora_url.to_owned());

        let node = node.build().unwrap();
        LDKNodeHandler { node }
    }
}

impl ExternalHandler for LDKNodeHandler {
    fn handle(
        &self,
        req: &lampo_common::jsonrpc::Request<lampo_common::json::Value>,
    ) -> error::Result<Option<lampo_common::json::Value>> {
        let getinfo = CommandSupported::from_str(req.method.as_str())?;
        match getinfo {
            CommandSupported::GetInfo => {
                let info = response::GetInfo {
                    node_id: self.node.node_id().to_string(),
                    peers: self.node.list_peers().len(),
                    channels: self.node.list_channels().len(),
                    chain: self.node.config().network.to_string(),
                    alias: "none".to_owned(),
                    blockheight: 0,
                    lampo_dir: self.node.config().storage_dir_path,
                    address: self
                        .node
                        .listening_addresses()
                        .unwrap_or(vec![])
                        .into_iter()
                        .map(|addr| {
                            let socket_addr = addr.to_socket_addrs().unwrap().next().unwrap();
                            NetworkInfo {
                                address: socket_addr.ip().to_string(),
                                port: socket_addr.port() as u64,
                            }
                        })
                        .collect(),
                };
                Ok(Some(json::to_value(&info)?))
            }
        }
    }
}
