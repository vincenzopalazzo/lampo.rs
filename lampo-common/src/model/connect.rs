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
    pub fn node_id(&self) -> error::Result<NodeId> {
        let result = NodeId::from_str(&self.node_id);
        match result {
            Ok(res) => Ok(res),
            Err(e) => Err(e.into()),
        }
    }

    pub fn addr(&self) -> error::Result<SocketAddr> {
        let addr = format!("{}:{}", self.addr, self.port);
        let result = SocketAddr::from_str(&addr);
        match result {
            Ok(res) => Ok(res),
            Err(e) => Err(e.into()),
        }
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
