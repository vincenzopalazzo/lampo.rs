use jsonrpsee::core::client::ClientT;
use jsonrpsee::ws_client::WsClient;
use jsonrpsee::ws_client::WsClientBuilder;
use serde::de::DeserializeOwned;
use serde::Serialize;

use lampo_common::error;
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
        let response = self.inner.request(method, json::to_value(input)?).await?;
        Ok(response)
    }
}
