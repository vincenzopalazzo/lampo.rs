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
    }
}

pub mod response {
    use std::vec::Vec;

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
}
