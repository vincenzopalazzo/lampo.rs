pub mod request {}

pub mod response {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct Utxo {
        pub txid: String,
        pub vout: u32,
        pub reserved: bool,
    }
}
