use std::str::FromStr;

pub use bitcoin::Network;
use clightningrpc_conf::{CLNConf, SyncCLNConf};
pub use lightning::util::config::UserConfig;

#[derive(Clone)]
pub struct LampoConf {
    pub inner: CLNConf,
    pub network: Network,
    pub ldk_conf: UserConfig,
    pub port: u64,
    pub path: String,
    pub private_key: Option<String>,
}

impl TryFrom<String> for LampoConf {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let path = format!("{value}/lampo.conf");
        let mut conf = CLNConf::new(path.clone(), false);
        conf.parse()
            .map_err(|err| anyhow::anyhow!("{}", err.cause))?;

        let Some(network) = conf.get_conf("network") else {
            anyhow::bail!("Network inside the configuration file missed");
        };
        let Some(network) = network.first() else {
            anyhow::bail!("this is a bug inside the configuration parser, the vector of values is empty!");
        };
        let Some(port) = conf.get_conf("port") else {
            anyhow::bail!("Port need to be specified inside the file");
        };
        let Some(port) = port.first() else {
            anyhow::bail!("this is a bug inside the configuration parser, the vector of values is empty!");
        };
        let private_key = conf
            .get_conf("dev-private-key")
            .map(|confs| confs.first().cloned().unwrap());
        Ok(Self {
            inner: conf,
            path: value,
            network: Network::from_str(network)?,
            ldk_conf: UserConfig::default(),
            port: u64::from_str(port)?,
            private_key,
        })
    }
}

impl LampoConf {
    pub fn path(&self) -> String {
        self.path.clone()
    }

    pub fn get_values(&self, key: &str) -> Option<Vec<String>> {
        self.inner.get_conf(key)
    }

    pub fn get_value(&self, key: &str) -> Option<String> {
        let Some(values) = self.inner.get_conf(key) else {
            return None;
        };
        let Some(value) = values.first() else {
            panic!("error inside the parse library, the vector of values is null");
        };
        Some(value.to_owned())
    }

    pub fn set_network(&mut self, network: &str) -> anyhow::Result<()> {
        self.network = Network::from_str(network)?;
        Ok(())
    }
}
