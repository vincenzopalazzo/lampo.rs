pub mod request {
    use std::num::ParseIntError;
    use std::str::FromStr;

    use bitcoin::secp256k1::PublicKey;
    use serde::{Deserialize, Serialize};

    use crate::error;
    use crate::types::*;

    #[derive(Clone, Serialize, Deserialize)]
    pub struct CloseChannel {
        pub counterpart_node_id: String,
        // Hex of the channel
        pub channel_id: String,
    }

    impl CloseChannel {
        pub fn counterpart_node_id(&self) -> error::Result<PublicKey> {
            let node_id = PublicKey::from_str(&self.counterpart_node_id)?;
            Ok(node_id)
        }

        // Returns ChannelId in byte format from hex of channelid
        pub fn channel_id(&self) -> error::Result<ChannelId> {
            let result = self.decode_hex(&self.channel_id)?;
            let mut result_array: [u8; 32] = [0; 32];
            for i in 0..32 {
                result_array[i] = result[i]
            }
            Ok(ChannelId::from_bytes(result_array))
        }

        fn decode_hex(&self, s: &str) -> Result<Vec<u8>, ParseIntError> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
                .collect()
        }
    }
}

pub mod response {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug)]
    pub struct CloseChannel {
        pub channel_id: String,
        pub message: String,
        pub counterparty_node_id: String,
        pub funding_utxo: String,
    }
}
