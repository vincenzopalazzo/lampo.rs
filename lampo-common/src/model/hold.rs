//! Model for the hold-payment (hodl invoice) stuff
pub mod request {
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct HoldInvoice {
        /// Hex encoded 32 byte payment hash, i.e. sha256 of a preimage
        /// that is known only by the caller.
        pub payment_hash: String,
        pub amount_msat: Option<u64>,
        pub description: String,
        pub expiring_in: Option<u32>,
        /// Minimum cltv delta for the final hop of the payment. This
        /// bounds how long (in blocks) the payment can be held before
        /// it is failed back automatically.
        pub min_final_cltv_expiry_delta: Option<u16>,
    }

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct HoldClaim {
        /// Hex encoded 32 byte payment preimage.
        pub payment_preimage: String,
    }

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct HoldFail {
        pub payment_hash: String,
    }

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct ListHolds {}

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct PaymentPreimage {
        /// Hex 32 byte payment hash of an outbound payment.
        pub payment_hash: String,
    }
}

pub mod response {
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct HoldInvoiceResult {
        pub bolt11: String,
        pub payment_hash: String,
    }

    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Apiv2Schema)]
    pub enum HoldStatus {
        /// The invoice has been issued but nothing has been received yet.
        Open,
        /// The payment arrived and the HTLCs are kept pending, waiting
        /// for the caller to claim or fail it.
        Held,
    }

    #[derive(Serialize, Deserialize, Debug, Clone, Apiv2Schema)]
    pub struct Hold {
        pub payment_hash: String,
        pub status: HoldStatus,
        /// The amount the invoice asks for, when it has one.
        pub expected_amount_msat: Option<u64>,
        /// The amount actually received, known once the payment is held.
        pub held_amount_msat: Option<u64>,
        /// Block height at which the held HTLCs are failed back
        /// automatically, known once the payment is held.
        pub claim_deadline: Option<u32>,
    }

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct ListHoldsResult {
        pub holds: Vec<Hold>,
    }

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct HoldClaimResult {
        pub payment_hash: String,
    }

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct HoldFailResult {
        pub payment_hash: String,
    }

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct PaymentPreimageResult {
        /// Hex preimage if the outbound payment settled and the node
        /// still holds its receipt; `None` otherwise (unknown, pending,
        /// or failed — the caller cannot tell those apart).
        pub payment_preimage: Option<String>,
    }
}
