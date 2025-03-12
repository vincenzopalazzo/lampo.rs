use std::io::Cursor;

use elite_rpc::protocol::Protocol;
use elite_rpc::transport::curl::HttpTransport;
use elite_rpc::transport::TransportMethod;
use elite_rpc::EliteRPC;

use lampo_common::error;
use lampo_common::handler::ExternalHandler;
use lampo_common::json;
use lampo_common::jsonrpc::Request;

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

#[derive(Clone)]
pub struct RestProtocol;

impl Protocol for RestProtocol {
    type InnerType = json::Value;

    fn new() -> error::Result<Self> {
        Ok(Self)
    }

    fn to_request(
        &self,
        url: &str,
        req: &Self::InnerType,
    ) -> error::Result<(String, Self::InnerType)> {
        Ok((url.to_string(), req.clone()))
    }

    fn from_request(
        &self,
        content: &[u8],
        _: std::option::Option<elite_rpc::protocol::Encoding>,
    ) -> error::Result<<Self as elite_rpc::protocol::Protocol>::InnerType> {
        let cursor = Cursor::new(content);
        let response: json::Value = json::from_reader(cursor)?;
        Ok(response)
    }
}
