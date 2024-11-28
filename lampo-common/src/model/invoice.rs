//! Model for the invoice stuff

pub mod request {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug)]
    pub struct GenerateInvoice {
        pub amount_msat: Option<u64>,
        pub description: String,
        pub expiring_in: Option<u32>,
    }

    #[derive(Serialize, Deserialize, Debug)]
    pub struct GenerateOffer {
        pub amount_msat: Option<u64>,
        pub description: Option<String>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct DecodeInvoice {
        pub invoice_str: String,
    }

    #[derive(Serialize, Deserialize)]
    pub struct Pay {
        pub invoice_str: String,
        pub amount: Option<u64>,
    }
}

pub mod response {
    use std::vec::Vec;

    use lightning::routing::router::RouteHop;
    use serde::{Deserialize, Serialize};

    use crate::ldk;

    #[derive(Serialize, Deserialize, Debug)]
    pub struct Invoice {
        pub bolt11: String,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Offer {
        pub bolt12: String,
        pub metadata: Option<String>,
        pub metadata_pubkey: Option<lightning::bitcoin::secp256k1::PublicKey>,
    }

    impl From<ldk::offers::offer::Offer> for Offer {
        fn from(value: ldk::offers::offer::Offer) -> Self {
            Self {
                bolt12: value.to_string(),
                metadata: value.metadata().map(hex::encode),
                metadata_pubkey: value.signing_pubkey(),
            }
        }
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct InvoiceInfo {
        pub issuer_id: Option<String>,
        pub expiry_time: Option<u64>,
        pub description: Option<String>,
        pub routes: Vec<String>,
        pub hints: Vec<String>,
        pub network: String,
        pub amount_msat: Option<u64>,
    }

    #[derive(Serialize, Deserialize, Debug)]
    pub struct PayResult {
        pub path: Vec<PaymentHop>,
        pub payment_hash: Option<String>,
        pub state: PaymentState,
        // FIXME: missing payment preimage
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub enum PaymentState {
        Success,
        Pending,
        Failure,
    }

    #[derive(Clone, Serialize, Deserialize, Debug)]
    pub struct PaymentHop {
        pub node_id: String,
        pub short_channel_id: u64,
        pub hop_fee_msat: u64,
        pub cltv_expiry_delta: u32,
        pub private_hop: bool,
    }

    impl From<RouteHop> for PaymentHop {
        fn from(value: RouteHop) -> Self {
            Self {
                node_id: value.pubkey.to_string(),
                short_channel_id: value.short_channel_id,
                hop_fee_msat: value.fee_msat,
                cltv_expiry_delta: value.cltv_expiry_delta,
                private_hop: value.maybe_announced_channel,
            }
        }
    }
}
