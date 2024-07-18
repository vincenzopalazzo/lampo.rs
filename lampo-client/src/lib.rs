use std::fmt::Display;

use async_trait::async_trait;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::core::params::ObjectParams;
use jsonrpsee::rpc_params;
use jsonrpsee::ws_client::WsClient;
use jsonrpsee::ws_client::WsClientBuilder;
use serde::de::DeserializeOwned;
use serde::Serialize;

use lampo_common::error;
use lampo_common::handler::ExternalHandler;
use lampo_common::json;

pub struct LampoClient {
    inner: WsClient,
}

impl LampoClient {
    pub async fn new(info: &str) -> error::Result<Self> {
        let client = WsClientBuilder::default().build(&info).await?;
        Ok(Self { inner: client })
    }

    pub async fn call<T: Serialize, U: DeserializeOwned>(
        &self,
        method: &str,
        input: T,
    ) -> error::Result<U> {
        log::info!(target: "ws-client", "call for `{method}`");
        let response = self.inner.request(method, json::to_value(input)?).await?;
        log::info!(target: "ws-client", "response received");
        Ok(response)
    }
}

#[async_trait]
impl ExternalHandler for LampoClient {
    async fn handle(&self, method: &str, body: &json::Value) -> error::Result<Option<json::Value>> {
        let response: json::Value = self.call(method, body).await?;
        Ok(Some(response))
    }
}
