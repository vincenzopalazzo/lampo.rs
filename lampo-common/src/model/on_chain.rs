pub mod request {}

pub mod response {
    use serde::{Deserialize, Serialize};

    use crate::bitcoin::Txid;

    #[derive(Serialize, Deserialize)]
    pub struct Utxo {
        pub txid: Txid,
        pub vout: u32,
        pub reserved: bool,
    }
}
