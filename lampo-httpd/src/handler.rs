use elite_rpc::transport::curl::HttpTransport;
use elite_rpc::transport::TransportMethod;
use elite_rpc::EliteRPC;

use lampo_common::error;
use lampo_common::handler::ExternalHandler;
use lampo_common::json;
use lampo_common::jsonrpc::Request;

use crate::rest_protocol::RestProtocol;

pub struct HttpdHandler {
    inner: EliteRPC<HttpTransport<RestProtocol>, RestProtocol>,
}

impl HttpdHandler {
    pub fn new(host: String) -> error::Result<Self> {
        let inner = EliteRPC::new(&host)?;
        Ok(Self { inner })
    }
}

impl ExternalHandler for HttpdHandler {
    fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>> {
        let method = req.method.clone();
        let body = req.params.clone();
        let response = self.inner.call(TransportMethod::Post(method), &body)?;
        Ok(Some(response))
    }
}
