//! Connect Model
use std::net::{SocketAddr, ToSocketAddrs};
use std::str::FromStr;

use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};

use super::request::OpenChannel;
use crate::error;
use crate::types::NodeId;

#[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
pub struct Connect {
    pub node_id: String,
    pub addr: String,
    pub port: u64,
}

impl Connect {
    pub fn node_id(&self) -> error::Result<NodeId> {
        Ok(NodeId::from_str(&self.node_id)?)
    }

    pub fn addr(&self) -> error::Result<SocketAddr> {
        let port = u16::try_from(self.port)
            .map_err(|_| error::anyhow!("peer port must be between 1 and 65535"))?;
        if port == 0 {
            error::bail!("peer port must be between 1 and 65535");
        }
        (self.addr.as_str(), port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| error::anyhow!("peer address did not resolve"))
    }
}

#[cfg(test)]
mod tests {
    use super::Connect;

    #[test]
    fn resolves_hostname_and_unbracketed_ipv6() {
        let hostname = Connect {
            node_id: String::new(),
            addr: "localhost".into(),
            port: 9735,
        };
        assert_eq!(hostname.addr().unwrap().port(), 9735);

        let ipv6 = Connect {
            node_id: String::new(),
            addr: "::1".into(),
            port: 9735,
        };
        assert!(ipv6.addr().unwrap().is_ipv6());
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
