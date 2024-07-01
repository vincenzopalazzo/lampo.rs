//! Open Channel Request

pub mod request {
    use std::str::FromStr;

    use serde::{Deserialize, Serialize};

    use crate::error;
    use crate::types::NodeId;

    #[derive(Clone, Serialize, Deserialize)]
    pub struct OpenChannel {
        pub node_id: String,
        pub addr: Option<String>,
        pub port: Option<u64>,
        pub amount: u64,
        pub public: bool,
    }

    impl OpenChannel {
        pub fn node_id(&self) -> error::Result<NodeId> {
            let node_id = NodeId::from_str(&self.node_id)?;
            Ok(node_id)
        }
    }
}

pub mod response {
    use std::str::FromStr;

    use serde::{Deserialize, Serialize};

    use crate::bitcoin::{Transaction, Txid};
    use crate::error;
    use crate::types::NodeId;

    #[derive(Serialize, Deserialize, Debug, Clone)]
    pub struct Channels {
        pub channels: Vec<Channel>,
    }

    #[derive(Serialize, Deserialize)]
    pub struct OpenChannel {
        pub node_id: String,
        pub amount: u64,
        pub public: bool,
        pub push_msat: u64,
        pub to_self_delay: u64,
        pub tx: Option<Transaction>,
        pub txid: Option<Txid>,
    }

    impl OpenChannel {
        pub fn node_id(&self) -> error::Result<NodeId> {
            let node_id = NodeId::from_str(&self.node_id)?;
            Ok(node_id)
        }
    }

    #[derive(Clone, Serialize, Deserialize, Debug)]
    pub struct Channel {
        // Channel_id needs to be string as it currently does not derive Serialize
        pub channel_id: String,
        pub short_channel_id: Option<u64>,
        pub peer_id: String,
        pub peer_alias: Option<String>,
        pub ready: bool,
        pub amount: u64,
        pub amount_msat: u64,
        pub public: bool,
        pub available_balance_for_send_msat: u64,
        pub available_balance_for_recv_msat: u64,
    }
}
