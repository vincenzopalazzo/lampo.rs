use std::str::FromStr;

use clightningrpc_conf::{CLNConf, SyncCLNConf};

pub use bitcoin::Network;
pub use lightning::util::config::UserConfig;

#[derive(Clone, Debug)]
pub struct LampoConf {
    pub inner: Option<CLNConf>,
    pub network: Network,
    pub ldk_conf: UserConfig,
    pub port: u64,
    pub path: String,
    /// The backend implementation
    pub node: String,
    pub core_url: Option<String>,
    pub core_user: Option<String>,
    pub core_pass: Option<String>,
    pub private_key: Option<String>,
    pub channels_keys: Option<String>,
}

impl LampoConf {
    // Create a new LampoConf with default values (This is used if the user doesn't specify a path)
    pub fn default() -> Self {
        // default path for the configuration file
        // (use deprecated std::env::home_dir() to avoid a dependency on dirs)
        #[allow(deprecated)]
        let path = std::env::home_dir().expect("Impossible to get the home directory");
        let path = path.to_str().unwrap();
        let lampo_home = format!("{}/.lampo", path);
        let lampo_testnet_path = format!("{}/testnet", lampo_home);
        Self {
            inner: None,
            // default network is testnet
            network: Network::Testnet,
            ldk_conf: UserConfig::default(),
            // default port is 19735 for testnet
            port: 19735,
            path: lampo_testnet_path,
            node: "core".to_owned(),
            core_url: None,
            core_user: None,
            core_pass: None,
            private_key: None,
            channels_keys: None,
        }
    }

    pub fn new(path: &str, network: Network, port: u64) -> Self {
        Self {
            inner: Some(CLNConf::new(format!("{path}/lampo.conf"), true)),
            network,
            ldk_conf: UserConfig::default(),
            port,
            path: path.to_string(),
            node: "core".to_owned(),
            core_url: None,
            core_user: None,
            core_pass: None,
            private_key: None,
            channels_keys: None,
        }
    }
}

impl TryFrom<String> for LampoConf {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        // if the path doesn't exist, return an error
        if !std::path::Path::new(&value).exists() {
            anyhow::bail!("The path {} doesn't exist", value);
        }

        let path = format!("{value}/lampo.conf");
        // Check for double slashes
        let path = path.replace("//", "/");

        // If lampo.conf doesn't exist, return the default configuration
        if !std::path::Path::new(&path).exists() {
            return Ok(Self::default());
        }

        let mut conf = CLNConf::new(path, false);
        conf.parse()
            .map_err(|err| anyhow::anyhow!("{}", err.cause))?;

        let Some(network) = conf
            .get_conf("network")
            .map_err(|err| anyhow::anyhow!("{err}"))?
        else {
            anyhow::bail!("Network inside the configuration file missed");
        };
        let Some(port) = conf
            .get_conf("port")
            .map_err(|err| anyhow::anyhow!("{err}"))?
        else {
            anyhow::bail!("Port need to be specified inside the file");
        };

        let node = conf
            .get_conf("backend")
            .map_err(|err| anyhow::anyhow!("{err}"))?
            .unwrap_or("nakamoto".to_owned());
        // Strip the value of whitespace
        let node = node.to_trimmed();

        let core_url = conf
            .get_conf("core-url")
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        // If the value isn't none, strip the value of whitespace
        let core_url = core_url.map(|url| url.to_trimmed());

        let core_user = conf
            .get_conf("core-user")
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let core_user = core_user.map(|user| user.to_trimmed());

        let core_pass = conf
            .get_conf("core-pass")
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let core_pass = core_pass.map(|pass| pass.to_trimmed());

        // Dev options
        #[allow(unused_mut, unused_assignments)]
        let mut private_key: Option<String> = None;
        #[allow(unused_mut, unused_assignments)]
        let mut channels_keys: Option<String> = None;

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
            inner: Some(conf),
            path: value,
            network: Network::from_str(&network)?,
            ldk_conf: UserConfig::default(),
            port: u64::from_str(&port)?,
            node,
            core_url,
            core_user,
            core_pass,
            private_key,
            channels_keys,
        })
    }
}

impl LampoConf {
    pub fn path(&self) -> String {
        self.path.clone()
    }

    pub fn get_values(&self, key: &str) -> Option<Vec<String>> {
        match self.inner {
            Some(ref conf) => Some(conf.get_confs(key)),
            None => None,
        }
    }

    pub fn get_value(&self, key: &str) -> Result<Option<String>, anyhow::Error> {
        let conf = self
            .inner
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Lampo configuration was not loaded"))?;

        let Some(value) = conf.get_conf(key).map_err(|err| anyhow::anyhow!("{err}"))? else {
            return Ok(None);
        };
        Ok(Some(value))
    }

    pub fn set_network(&mut self, network: &str) -> anyhow::Result<()> {
        self.network = Network::from_str(network)?;
        Ok(())
    }
}

// A trait to trim a String
trait TrimmedString {
    fn to_trimmed(self) -> String;
}

impl TrimmedString for String {
    fn to_trimmed(self) -> String {
        self.trim().to_owned()
    }
}
