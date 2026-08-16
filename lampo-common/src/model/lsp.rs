use paperclip::actix::Apiv2Schema;
use serde::{Deserialize, Serialize};

pub mod request {
    use super::*;

    /// Request body for `lsps0-list-protocols`.
    #[derive(Serialize, Deserialize, Debug, Apiv2Schema, Clone)]
    pub struct ListProtocols {
        pub node_id: String,
    }
}

pub mod response {
    use super::*;

    /// Status of the experimental LSP plugin.
    #[derive(Serialize, Deserialize, Debug, Apiv2Schema, Clone)]
    pub struct LspInfo {
        pub enabled: bool,
        pub client: bool,
        pub service: bool,
        pub advertise: bool,
        /// Always true: upstream service-side support is beta.
        pub experimental: bool,
    }

    /// Response body for `lsps0-list-protocols`.
    #[derive(Serialize, Deserialize, Debug, Apiv2Schema, Clone)]
    pub struct Protocols {
        pub protocols: Vec<u16>,
    }
}
