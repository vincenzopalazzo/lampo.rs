//! Model for the invoice stuff

pub mod request {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct GenerateInvoice {
        pub amount_msat: Option<u64>,
        pub description: String,
        pub expiring_in: Option<u32>,
    }

    #[derive(Serialize, Deserialize)]
    pub struct DecodeInvoice {
        pub invoice_str: String,
        pub amount: Option<u64>,
    }
}

pub mod response {
    use std::vec::Vec;

    use lightning::routing::router::RouteHop;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct Invoice {
        pub bolt11: String,
    }

    #[derive(Serialize, Deserialize)]
    pub struct InvoiceInfo {
        pub expiry_time: u64,
        pub description: String,
        pub routes: Vec<String>,
        pub hints: Vec<String>,
        pub network: String,
        pub amount_msa: Option<u64>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub enum PaymentState {
        Success,
        Pending,
        Faulure,
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
