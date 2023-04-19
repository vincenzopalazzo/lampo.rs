//! Connect Model
use std::{net::SocketAddr, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::types::NodeId;

#[derive(Serialize, Deserialize, Debug)]
pub struct Connect {
    pub node_id: String,
    pub addr: String,
    pub port: u64,
}

impl Connect {
    pub fn node_id(&self) -> NodeId {
        NodeId::from_str(&self.node_id).unwrap()
    }

    pub fn addr(&self) -> SocketAddr {
        let addr = format!("{}:{}", self.addr, self.port);
        SocketAddr::from_str(&addr).unwrap()
    }
}
