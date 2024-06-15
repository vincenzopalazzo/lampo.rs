use std::sync::Arc;

use lampo_common::vls::async_trait::async_trait;
use lampo_common::vls::proxy::vls_protocol_client::{Error, SignerPort, Transport};

#[allow(dead_code)]
/// Wraps the LampoVLSInProcess in a struct,
/// providing a structured way to interact with the protocol asynchronously via the SignerPort trait.
pub struct LampoVLSSignerPort {
    protocol_handler: Arc<dyn Transport>,
}

impl LampoVLSSignerPort {
    pub fn new(protocol_handler: Arc<dyn Transport>) -> Self {
        LampoVLSSignerPort { protocol_handler }
    }
}

#[async_trait]
impl SignerPort for LampoVLSSignerPort {
    async fn handle_message(&self, message: Vec<u8>) -> Result<Vec<u8>, Error> {
        self.protocol_handler.node_call(message)
    }
    fn is_ready(&self) -> bool {
        true
    }
}
