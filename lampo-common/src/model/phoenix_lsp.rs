//! Phoenix LSP models
pub mod request {}

pub mod response {
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    /// A liquidity purchase this node paid, or will pay, the LSP for.
    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Apiv2Schema)]
    pub struct PhoenixLspPurchase {
        pub funding_txid: String,
        pub amount_sat: u64,
        pub mining_fee_sat: u64,
        pub service_fee_sat: u64,
        /// Liquidity ads payment type bit: 0, 128, 129 or 130.
        pub payment_type: u32,
        /// Hex payment hashes, or hex preimages for payment type 129.
        #[serde(default)]
        pub payment_hashes: Vec<String>,
        #[serde(default)]
        pub fee_credit_used_msat: u64,
        pub created_at: u64,
    }
}
