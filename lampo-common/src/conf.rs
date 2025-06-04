use std::str::FromStr;

use bitcoin::absolute::Height;
use clightningrpc_conf::{CLNConf, SyncCLNConf};

pub use bitcoin::Network;
pub use lightning::util::config::UserConfig;

#[derive(Clone, Debug)]
pub struct LampoConf {
    pub inner: Option<CLNConf>,
    pub network: Network,
    pub ldk_conf: UserConfig,
    pub port: u64,
    pub root_path: String,
    /// The backend implementation
    pub node: String,
    pub core_url: Option<String>,
    pub core_user: Option<String>,
    pub core_pass: Option<String>,
    pub private_key: Option<String>,
    pub channels_keys: Option<String>,
    pub log_file: Option<String>,
    pub log_level: String,
    pub alias: Option<String>,
    pub announce_addr: Option<String>,
    pub api_host: String,
    pub api_port: u64,
    pub reindex: Option<Height>,
    pub dev_sync: Option<bool>,
}

impl Default for LampoConf {
    fn default() -> Self {
        // default path for the configuration file
        // (use deprecated std::env::home_dir() to avoid a dependency on dirs)
        #[allow(deprecated)]
        let path = std::env::home_dir().expect("Impossible to get the home directory");
        let path = path.to_str().unwrap();
        let lampo_home = format!("{}/.lampo", path);
        Self {
            inner: None,
            // default network is testnet
            network: Network::Testnet,
            ldk_conf: UserConfig::default(),
            // default port is 19735 for testnet
            port: 19735,
            root_path: lampo_home,
            node: "core".to_owned(),
            core_url: None,
            core_user: None,
            core_pass: None,
            private_key: None,
            channels_keys: None,
            log_level: "info".to_string(),
            log_file: None,
            alias: None,
            announce_addr: None,
            api_host: "127.0.0.1".to_owned(),
            api_port: 7878,
            reindex: None,
            dev_sync: None,
        }
    }
}

impl LampoConf {
    pub fn prepare_dirs(&self) -> Result<(), anyhow::Error> {
        Self::prepare_directories(&self.root_path, Some(self.network))
    }

    pub fn prepare_directories(
        root_path: &str,
        network: Option<Network>,
    ) -> Result<(), anyhow::Error> {
        let root_path = Self::normalize_root_dir(root_path, network.unwrap_or(Network::Testnet));
        // make sure that the data-dir exist
        if !std::path::Path::new(&root_path).exists() {
            log::info!("Creating root dir at `{}`", root_path);
            std::fs::create_dir(root_path.clone())?;
        }

        if let Some(network) = network {
            let network_path = format!("{root_path}/{network}");
            if !std::path::Path::new(&network_path).exists() {
                log::info!("Creating network directory at `{network_path}`");
                std::fs::create_dir(network_path)?;
            }
        }
        Ok(())
    }
    // Sometimes the root path is given already with the network
    // e.g: when we read the datadir from the cli args we do not have
    // any way to get the network from the string (because it contains the root)
    #[inline(always)]
    pub fn normalize_root_dir(root_path: &str, network: Network) -> String {
        let suffix_with_slash = format!("/{network}/");
        let suffix_without_slash = format!("/{network}");

        let root = if root_path.ends_with(&suffix_with_slash)
            || root_path.ends_with(&suffix_without_slash)
        {
            root_path
                .trim_end()
                .strip_suffix(&suffix_with_slash)
                // SAFETY: we make a check before inside the if condition
                // so it is safe unwrap here otherwise we are hiding a bug
                // and we must crash.
                .or_else(|| root_path.strip_suffix(&suffix_without_slash))
                .unwrap_or_else(|| panic!("path: {root_path} - network: {network}"))
                .to_owned()
        } else {
            root_path.to_owned()
        };
        log::trace!("normalize root path {root} for network {network}");
        root
    }

    pub fn new(
        path: Option<String>,
        network: Option<Network>,
        port: Option<u64>,
    ) -> Result<Self, anyhow::Error> {
        let mut conf = Self::default();
        conf.network = network.unwrap_or(conf.network);
        conf.port = port.unwrap_or(conf.port);
        conf.root_path = path.clone().unwrap_or(conf.root_path);
        Self::prepare_directories(&conf.root_path, Some(conf.network))?;
        let input_path = path;
        let path = Self::normalize_root_dir(&conf.root_path, conf.network);
        conf.root_path = path.clone();

        let lampo_file = format!("{}/lampo.conf", conf.path());

        if std::fs::File::open(lampo_file.clone()).is_ok() {
            let mut conf = Self::try_from(conf.path())?;
            conf.network = network.unwrap_or(conf.network);
            conf.port = port.unwrap_or(conf.port);
            conf.root_path = input_path.unwrap_or(conf.root_path);
            return Ok(conf);
        }

        Ok(conf)
    }
}

impl TryFrom<String> for LampoConf {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let path = format!("{value}/lampo.conf");
        // Check for double slashes
        let path = path.replace("//", "/");

        // If lampo.conf doesn't exist, return the default configuration
        if !std::path::Path::new(&path).exists() {
            anyhow::bail!("Configuration file not found at `{path}`");
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

        let mut core_url = None;
        let mut core_user = None;
        let mut core_pass = None;
        if node == "core" {
            core_url = conf
                .get_conf("core-url")
                .map_err(|err| anyhow::anyhow!("{err}"))?;
            // If the value isn't none, strip the value of whitespace
            core_url = core_url.map(|url| url.to_trimmed());

            core_user = conf
                .get_conf("core-user")
                .map_err(|err| anyhow::anyhow!("{err}"))?;
            core_user = core_user.map(|user| user.to_trimmed());

            core_pass = conf
                .get_conf("core-pass")
                .map_err(|err| anyhow::anyhow!("{err}"))?;
            core_pass = core_pass.map(|pass| pass.to_trimmed());
        }

        let reindex: Option<String> = conf
            .get_conf("reindex")
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let reindex = if let Some(reindex) = reindex {
            let reindex = Height::from_str(&reindex)?;
            Some(reindex)
        } else {
            None
        };
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

        let network = Network::from_str(&network)?;
        let root_path = Self::normalize_root_dir(&value, network);
        let log_level = conf.get_conf("log-level");
        let level = match log_level {
            Ok(Some(level)) => level,
            _ => "info".to_string(),
        };
        let log_file = conf.get_conf("log-file").unwrap_or(None);
        let alias = conf.get_conf("alias").unwrap_or(None);
        let announce_addr = conf.get_conf("announce-addr").unwrap_or(None);
        let api_host = conf.get_conf("api-host").unwrap_or(None);
        let api_port = conf.get_conf("api-port").unwrap_or(None);
        let api_host = api_host.unwrap_or("http://127.0.0.1".to_owned());
        let api_port: u64 = api_port.unwrap_or("7979".to_owned()).parse()?;

        // Parse dev_sync field - defaults to None (false)
        let dev_sync = conf
            .get_conf("dev-sync")
            .unwrap_or(None)
            .map(|s| s.to_lowercase() == "true" || s == "1");
        Ok(Self {
            inner: Some(conf),
            root_path,
            network,
            ldk_conf: UserConfig::default(),
            port: u64::from_str(&port)?,
            node,
            core_url,
            core_user,
            core_pass,
            private_key,
            channels_keys,
            log_file,
            log_level: level,
            alias,
            announce_addr,
            api_host,
            api_port,
            reindex,
            dev_sync,
        })
    }
}

impl LampoConf {
    pub fn path(&self) -> String {
        format!("{}/{}", self.root_path, self.network)
    }

    pub fn get_values(&self, key: &str) -> Option<Vec<String>> {
        self.inner.as_ref().map(|conf| conf.get_confs(key))
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
