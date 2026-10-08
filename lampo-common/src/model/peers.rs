//! Peers model
pub mod response {
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct Peers {
        pub peers: Vec<Peer>,
    }

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct Peer {
        pub node_id: String,
        /// The address of the connection, if known.
        pub address: Option<String>,
        /// Whether the peer connected to us.
        pub inbound: bool,
    }
}
