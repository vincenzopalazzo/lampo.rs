//! keysend model

pub mod request {
    use std::str::FromStr;

    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    use crate::bitcoin::secp256k1::PublicKey;
    use crate::error;
    use crate::model::pay_timeout::PayTimeout;
    #[derive(Serialize, Deserialize, Apiv2Schema)]
    pub struct KeySend {
        pub destination: String,
        pub amount_msat: u64,
        /// How long the RPC waits for the terminal `PaymentEvent` (`fast` / `medium` / `large`).
        #[serde(default)]
        pub timeout: PayTimeout,
    }

    impl KeySend {
        pub fn destination(&self) -> error::Result<PublicKey> {
            let destination = PublicKey::from_str(&self.destination)?;
            Ok(destination)
        }
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
