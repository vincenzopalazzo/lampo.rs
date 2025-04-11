use bitcoin::Network;
use clightningrpc_conf::CLNConf;

#[derive(Clone, Debug)]
pub struct BitcoindConf {
    url: String,
    user: String,
    pass: String,
}

// The default URLs for different bitcoin chains
pub static BITCOIN_MAINNET_URL: &str = "127.0.0.1:8332";
pub static BITCOIN_TESTNET_URL: &str = "127.0.0.1:18332";
pub static BITCOIN_SIGNET_URL: &str = "127.0.0.1:28332";
pub static BITCOIN_REGTEST_URL: &str = "127.0.0.1:38332";
pub static BITCOIN_USER: &str = "user";
pub static BITCOIN_PASS: &str = "pass";

impl BitcoindConf {
    pub fn get_default_conf(network: Network) -> Self {
        match network {
            Network::Bitcoin => BitcoindConf {
                url: BITCOIN_MAINNET_URL.to_string(),
                user: BITCOIN_USER.to_string(),
                pass: BITCOIN_PASS.to_string(),
            },
            Network::Testnet => BitcoindConf {
                url: BITCOIN_TESTNET_URL.to_string(),
                user: BITCOIN_USER.to_string(),
                pass: BITCOIN_PASS.to_string(),
            },
            Network::Signet => BitcoindConf {
                url: BITCOIN_SIGNET_URL.to_string(),
                user: BITCOIN_USER.to_string(),
                pass: BITCOIN_PASS.to_string(),
            },
            Network::Regtest => BitcoindConf {
                url: BITCOIN_REGTEST_URL.to_string(),
                user: BITCOIN_USER.to_string(),
                pass: BITCOIN_PASS.to_string(),
            },
            Network::Testnet4 => BitcoindConf {
                url: BITCOIN_TESTNET_URL.to_string(),
                user: BITCOIN_USER.to_string(),
                pass: BITCOIN_PASS.to_string(),
            },
            // Ideally we can no way enter here, as the network is checked whenever we are
            // calling this function and if it is not a valid network, the program flow
            // would not enter here.
            _ => panic!(
                "This shouldn't have happened, take a look at your bitcoin network just in case"
            ),
        }
    }

    pub fn get_url(&self) -> String {
        self.url.clone()
    }

    pub fn get_user(&self) -> String {
        self.user.clone()
    }

    pub fn get_pass(&self) -> String {
        self.pass.clone()
    }

    pub fn set_url(&mut self, url: String) {
        self.url = url
    }

    pub fn set_user(&mut self, user: String) {
        self.user = user
    }

    pub fn set_pass(&mut self, pass: String) {
        self.pass = pass
    }
}
