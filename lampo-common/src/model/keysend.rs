//! keysend model

pub mod request {
    use bitcoin::secp256k1::PublicKey;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct KeySend {
        pub destination: PublicKey,
        pub amount_msat: u64,
    }
}

pub mod response {

    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct KeySendInfo {
        pub payment_preimage: String,
        pub payment_hash: String,
        pub created_at: String,
        pub parts: String,
        pub amount_msat: String,
        pub amount_sent_msat: Option<u64>,
        pub status: String,
    }
}
