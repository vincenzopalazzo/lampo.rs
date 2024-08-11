use std::sync::Arc;

use async_trait::async_trait;
use vls_proxy::vls_protocol_client::{Error, SignerPort, Transport};

pub struct VLSSignerPort {
    protocol_handler: Arc<dyn Transport>,
}

impl VLSSignerPort {
    pub fn new(protocol_handler: Arc<dyn Transport>) -> Self {
        VLSSignerPort { protocol_handler }
    }
}

#[async_trait]
impl SignerPort for VLSSignerPort {
    async fn handle_message(&self, message: Vec<u8>) -> Result<Vec<u8>, Error> {
        self.protocol_handler.node_call(message)
    }
    fn is_ready(&self) -> bool {
        true
    }
}
