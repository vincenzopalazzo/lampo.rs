//! New address model
pub mod request {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct NewAddress;
}

pub mod response {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct NewAddress {
        pub address: String,
    }
}
