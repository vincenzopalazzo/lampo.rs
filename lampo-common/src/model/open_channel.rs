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

    use crate::error;
    use crate::types::NodeId;

    #[derive(Serialize, Deserialize)]
    pub struct OpenChannel {
        pub node_id: String,
        pub amount: u64,
        pub public: bool,
        pub push_mst: u64,
        pub to_self_delay: u64,
    }

    impl OpenChannel {
        pub fn node_id(&self) -> error::Result<NodeId> {
            let node_id = NodeId::from_str(&self.node_id)?;
            Ok(node_id)
        }
    }
}
