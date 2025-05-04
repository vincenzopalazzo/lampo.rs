use bitcoin::Network;

use crate::error;

#[derive(Clone, Debug)]
pub struct BitcoindConf {
    pub url: String,
    pub user: Option<String>,
    pub pass: Option<String>,
}

impl BitcoindConf {
    pub fn get_default_conf(network: Network) -> Result<Self, anyhow::Error> {
        match network {
            Network::Bitcoin => Ok(BitcoindConf {
                url: "127.0.0.1:8332".to_string(),
                user: None,
                pass: None,
            }),
            Network::Testnet => Ok(BitcoindConf {
                url: "127.0.0.1:18332".to_string(),
                user: None,
                pass: None,
            }),
            Network::Signet => Ok(BitcoindConf {
                url: "127.0.0.1:28332".to_string(),
                user: None,
                pass: None,
            }),
            Network::Regtest => Ok(BitcoindConf {
                url: "127.0.0.1:38332".to_string(),
                user: None,
                pass: None,
            }),
            Network::Testnet4 => Ok(BitcoindConf {
                url: "127.0.0.1:18332".to_string(),
                user: None,
                pass: None,
            }),
            _ => error::bail!("Network not supported"),
        }
    }
}
