use std::io::Cursor;
use std::sync::Arc;

use elite_rpc::protocol::Protocol;
use elite_rpc::transport::curl::HttpTransport;
use elite_rpc::transport::TransportMethod;
use elite_rpc::EliteRPC;

use lampo_common::async_trait;
use lampo_common::error;
use lampo_common::handler::ExternalHandler;
use lampo_common::json;
use lampo_common::jsonrpc::Request;

pub struct HttpdHandler {
    inner: Arc<EliteRPC<HttpTransport<RestProtocol>, RestProtocol>>,
}

impl HttpdHandler {
    pub fn new(host: String) -> error::Result<Self> {
        let inner = EliteRPC::new(&host)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }
}

#[async_trait]
impl ExternalHandler for HttpdHandler {
    async fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>> {
        // Plugin methods are not HTTP routes. Posting them back to
        // `/{method}` re-enters this process: the catch-all calls the
        // plugin again, which calls this handler again. Built-ins are
        // the typed routes. `getinfo` is one of them; `yoooo` is not.
        const BUILTIN: &[&str] = &[
            "getinfo",
            "networkchannels",
            "funds",
            "invoice",
            "offer",
            "decode",
            "pay",
            "keysend",
            "asyncinvoicepaths",
            "setasyncinvoicepaths",
            "new_addr",
            "connect",
            "close",
            "channels",
            "fundchannel",
            "stop",
        ];
        if !BUILTIN.contains(&req.method.as_str()) {
            return Ok(None);
        }
        let method = req.method.clone();
        let body = req.params.clone();
        let inner = self.inner.clone();
        let task = tokio::task::spawn_blocking(move || {
            let result = inner.call(TransportMethod::Post(method), &body);
            match result {
                Ok(response) => Ok(Some(response)),
                Err(e) => Err(e),
            }
        })
        .await;
        let task = task.map_err(|err| error::anyhow!("task error: {}", err))?;
        task
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
