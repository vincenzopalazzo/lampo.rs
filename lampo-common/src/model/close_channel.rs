pub mod request {
    use std::num::ParseIntError;
    use std::str::FromStr;

    use bitcoin::secp256k1::PublicKey;
    use serde::{Deserialize, Serialize};

    use crate::error;
    use crate::types::*;

    #[derive(Clone, Serialize, Deserialize)]
    pub struct CloseChannel {
        pub node_id: String,
        // Hex of the channel
        pub channel_id: Option<String>,
    }

    impl CloseChannel {
        pub fn counterpart_node_id(&self) -> error::Result<PublicKey> {
            let node_id = PublicKey::from_str(&self.node_id)?;
            Ok(node_id)
        }

        // Returns ChannelId in byte format from hex of channelid
        pub fn channel_id(&self) -> error::Result<ChannelId> {
            let id = self
                .channel_id
                .as_ref()
                .ok_or(error::anyhow!("`channel_id` not found"))?;
            let result = self.decode_hex(&id)?;
            let mut result_array: [u8; 32] = [0; 32];
            result_array.copy_from_slice(&result);
            Ok(ChannelId::from_bytes(result_array))
        }

        /// This converts hex to bytes array.
        /// Stolen from https://stackoverflow.com/a/52992629
        /// It takes two values every in each iteration from the hex
        /// then convert the formed hexdecimal digit to u8, collects it in a vector
        /// and return it (redix = 16 for hexadecimal)
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
        pub peer_id: String,
        pub funding_utxo: String,
    }
}

pub mod tests {

    #[test]
    fn channel_id_tests() {
        let node_id =
            "039c108cc6777e7d5066dfa33c611c32e6baa1c49de6d546b5b76686486d0360ac".to_string();

        // This is a correct channel_hex of 32 bytes
        let channel_hex =
            Some("0a44677526ac8c607616bd91258d7e5df1d86fae9c32e23aa18703a650944c64".to_string());
        let req = crate::model::request::CloseChannel {
            node_id: node_id.clone(),
            channel_id: channel_hex,
        };
        let channel_bytes = [
            10, 68, 103, 117, 38, 172, 140, 96, 118, 22, 189, 145, 37, 141, 126, 93, 241, 216, 111,
            174, 156, 50, 226, 58, 161, 135, 3, 166, 80, 148, 76, 100,
        ];
        let channel_id_bytes = req.channel_id();
        assert_eq!(channel_bytes, channel_id_bytes.unwrap().0);
    }
}
