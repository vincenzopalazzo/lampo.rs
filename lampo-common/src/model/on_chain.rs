pub mod request {}

pub mod response {
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, Apiv2Schema)]
    pub struct Utxo {
        pub txid: String,
        pub vout: u32,
        pub reserved: bool,
        pub confirmed: u32,
        pub amount_msat: u64,
    }

    #[derive(Debug, Serialize, Deserialize, Apiv2Schema)]
    pub struct Utxos {
        pub transactions: Vec<Utxo>,
    }
}
