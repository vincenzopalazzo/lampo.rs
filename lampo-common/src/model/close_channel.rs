pub mod request {
    use std::str::FromStr;

    use bitcoin::hashes::hex;
    use serde::{Deserialize, Serialize};

    use crate::error;
    use crate::types::*;

    #[derive(Clone, Serialize, Deserialize)]
    pub struct CloseChannel {
        pub counterpart_node_id: String,
        pub channel_id: String,
    }

    impl CloseChannel {
        pub fn counterpart_node_id(&self) -> error::Result<NodeId> {
            let node_id = NodeId::from_str(&self.counterpart_node_id)?;
            Ok(node_id)
        }

        pub fn channel_id(&self) -> error::Result<ChannelId> {
            let result: [u8; 32] = hex::FromHex::from_hex(&self.channel_id).unwrap();
            let channel_id = ChannelId::from_bytes(result);
            Ok(channel_id)
        }
    }
}

pub mod response {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct CloseChannel {
        pub channel_id: String,
    }
}
