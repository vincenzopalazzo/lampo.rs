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
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct Invoice {
        pub bolt11: String,
    }
}
