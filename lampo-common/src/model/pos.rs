//! BLIP-0056 Point-of-Sale models.

pub mod request {
    use bitcoin::secp256k1::PublicKey;
    use serde::{Deserialize, Serialize};

    /// Send a `payment_notification` onion message to a PoS node.
    ///
    /// This is the merchant-side send primitive. `payment_hash` and `preimage`
    /// are hex-encoded 32-byte values.
    ///
    /// NOTE: no `Apiv2Schema` derive because `PublicKey` does not implement it;
    /// the httpd `post!` macro deserializes the request from a generic JSON
    /// body, so only the response type needs the schema.
    #[derive(Serialize, Deserialize, Debug)]
    pub struct SendPaymentNotification {
        pub node_id: PublicKey,
        pub payment_hash: String,
        pub preimage: String,
        pub amount_msat: u64,
    }
}

pub mod response {
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct SendPaymentNotification {
        pub status: String,
    }
}
