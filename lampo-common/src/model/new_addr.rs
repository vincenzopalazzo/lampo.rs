//! New address model
pub mod request {
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Apiv2Schema)]
    pub struct NewAddress;
}

pub mod response {
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Apiv2Schema)]
    pub struct NewAddress {
        pub address: String,
    }
}
