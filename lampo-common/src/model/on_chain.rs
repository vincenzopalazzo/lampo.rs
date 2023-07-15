pub mod request {}

pub mod response {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Utxo {
        pub txid: String,
        pub vout: u32,
        pub reserved: bool,
        pub confirmed: u32,
        pub amount_msat: u64,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Utxos {
        pub transactions: Vec<Utxo>,
    }
}
