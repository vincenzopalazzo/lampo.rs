pub use clightningrpc_common::errors;

use clightningrpc_common::client;
use clightningrpc_common::errors::Error;
use lampo_common::error;
use serde::de::DeserializeOwned;
use serde::Serialize;

pub struct UnixClient {
    #[allow(dead_code)]
    socket_path: String,
    inner: client::Client,
}

impl UnixClient {
    pub fn new(path: &str) -> error::Result<Self> {
        let client = client::Client::new(path);
        Ok(Self {
            socket_path: path.to_string(),
            inner: client,
        })
    }

    pub fn call<T: Serialize, U: DeserializeOwned>(
        &self,
        method: &str,
        input: T,
    ) -> Result<U, Error> {
        let res = self
            .inner
            .send_request(method, input)
            .and_then(|res| res.into_result())?;
        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::Value;

    use lampo_common::logger;
    use lampo_common::model::Connect;

    use crate::UnixClient;

    #[test]
    #[ignore = "we need to run a node"]
    fn get_info_call() {
        let client = UnixClient::new("/home/vincent/.lampo/testnet/lampod.socket").unwrap();
        let input: HashMap<String, Value> = HashMap::new();
        log::debug!("input method: `{:?}`", input);
        let resp: HashMap<String, Value> = client.call("getinfo", input).unwrap();
        log::info!("get info response: `{:?}`", resp)
    }

    #[test]
    #[ignore = "we need to run a node"]
    fn connect_call() {
        logger::init("debug", None).unwrap();
        let client = UnixClient::new("/home/vincent/.lampo/testnet/lampod.socket").unwrap();
        let input = Connect {
            node_id: "02049b60c296ffead3e7c8b124c5730153403a8314c1116c2d1b43cf9ac0de2d9d"
                .to_string(),
            addr: "78.46.220.4".to_string(),
            port: 19735,
        };
        log::debug!("input method: `{:?}`", input);
        let resp: HashMap<String, Value> = client.call("connect", input).unwrap();
        log::info!("`connect` response: `{:?}`", resp)
    }
}
