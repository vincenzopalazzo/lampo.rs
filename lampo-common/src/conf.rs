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
    pub channels_keys: Option<String>,
}

impl TryFrom<String> for LampoConf {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let path = format!("{value}/lampo.conf");
        let mut conf = CLNConf::new(path.clone(), false);
        conf.parse()
            .map_err(|err| anyhow::anyhow!("{}", err.cause))?;

        let Some(network) = conf.get_conf("network").map_err(|err| anyhow::anyhow!("{err}"))? else {
            anyhow::bail!("Network inside the configuration file missed");
        };
        let Some(port) = conf.get_conf("port").map_err(|err| anyhow::anyhow!("{err}"))? else {
            anyhow::bail!("Port need to be specified inside the file");
        };
        let private_key: Option<String>;
        let channels_keys: Option<String>;

        #[cfg(debug_assertions)]
        {
            private_key = conf
                .get_conf("dev-private-key")
                .map_err(|err| anyhow::anyhow!("{err}"))?;

            channels_keys = conf
                .get_conf("dev-force-channel-secrets")
                .map_err(|err| anyhow::anyhow!("{err}"))?;
        }

        Ok(Self {
            inner: conf,
            path: value,
            network: Network::from_str(&network)?,
            ldk_conf: UserConfig::default(),
            port: u64::from_str(&port)?,
            private_key,
            channels_keys,
        })
    }
}

impl LampoConf {
    pub fn path(&self) -> String {
        self.path.clone()
    }

    pub fn get_values(&self, key: &str) -> Vec<String> {
        self.inner.get_confs(key)
    }

    pub fn get_value(&self, key: &str) -> Result<Option<String>, anyhow::Error> {
        let Some(value) = self.inner.get_conf(key).map_err(|err| anyhow::anyhow!("{err}"))? else {
            return Ok(None);
        };
        Ok(Some(value))
    }

    pub fn set_network(&mut self, network: &str) -> anyhow::Result<()> {
        self.network = Network::from_str(network)?;
        Ok(())
    }
}
