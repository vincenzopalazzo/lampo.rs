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
        #[serde(default)]
        pub push_msat: Option<u64>,
        #[serde(default)]
        pub sat_per_vbyte: Option<u64>,
        pub public: bool,
    }

    impl OpenChannel {
        pub fn node_id(&self) -> error::Result<NodeId> {
            let node_id = NodeId::from_str(&self.node_id)?;
            Ok(node_id)
        }
    }

    /// Add on-chain funds to an existing channel. `channel_id` is required
    /// when the peer has more than one ready channel.
    #[derive(Clone, Serialize, Deserialize)]
    pub struct SpliceIn {
        pub node_id: String,
        pub channel_id: Option<String>,
        pub amount: u64,
        #[serde(default)]
        pub sat_per_vbyte: Option<u64>,
    }

    /// Pay `address` from the channel balance without closing it.
    #[derive(Clone, Serialize, Deserialize)]
    pub struct SpliceOut {
        pub node_id: String,
        pub channel_id: Option<String>,
        pub amount: u64,
        pub address: String,
        #[serde(default)]
        pub sat_per_vbyte: Option<u64>,
    }

    /// Replace the unconfirmed splice on a channel with a higher feerate.
    #[derive(Clone, Serialize, Deserialize)]
    pub struct BumpChannelFee {
        pub node_id: String,
        pub channel_id: Option<String>,
        #[serde(default)]
        pub sat_per_vbyte: Option<u64>,
    }

    /// A channel id is 32 bytes, 64 hex characters. Odd lengths panic in a
    /// nibble decoder, and a short vector panics in `copy_from_slice`.
    pub fn parse_channel_id(id: &str) -> error::Result<crate::types::ChannelId> {
        if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(error::anyhow!(
                "invalid `channel_id`: expected 64 hex characters, got a string of length {}",
                id.len()
            ));
        }
        let bytes =
            crate::hex::decode(id).map_err(|err| error::anyhow!("invalid `channel_id`: {err}"))?;
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&bytes);
        Ok(crate::types::ChannelId::from_bytes(raw))
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

    /// Result of `splicein`, `spliceout`, and `bumpchannelfee`. `txid` is the
    /// negotiated funding transaction, not yet necessarily confirmed.
    #[derive(Serialize, Deserialize, Debug, Clone)]
    pub struct Splice {
        pub node_id: String,
        pub channel_id: String,
        pub txid: String,
        pub vout: u32,
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
        /// Whether we opened the channel.
        #[serde(default)]
        pub is_outbound: bool,
        /// The funding outpoint as `txid:vout`, once the funding transaction is known.
        #[serde(default)]
        pub funding_txo: Option<String>,
    }
}
