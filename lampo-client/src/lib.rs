use jsonrpsee::core::client::ClientT;
use jsonrpsee::rpc_params;
use jsonrpsee::ws_client::WsClient;
use jsonrpsee::ws_client::WsClientBuilder;
use serde::de::DeserializeOwned;
use serde::Serialize;

use lampo_common::error;

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
        let response = self.inner.request(method, rpc_params!(input)).await?;
        Ok(response)
    }
}
