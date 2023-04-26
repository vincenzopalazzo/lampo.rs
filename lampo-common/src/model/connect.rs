//! Connect Model
use std::{net::SocketAddr, str::FromStr};

use serde::{Deserialize, Serialize};

use super::request::OpenChannel;
use crate::error;
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

impl TryFrom<OpenChannel> for Connect {
    type Error = error::Error;

    fn try_from(value: OpenChannel) -> Result<Self, Self::Error> {
        Ok(Connect {
            node_id: value.node_id,
            addr: value
                .addr
                .ok_or(error::anyhow!("The `addr` must be specified"))?,
            port: value
                .port
                .ok_or(error::anyhow!("The `port` must be specifed"))?,
        })
    }
}
