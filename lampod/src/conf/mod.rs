pub use bitcoin::Network;
pub use lightning::util::config::UserConfig;

#[derive(Clone)]
pub struct LampoConf {
    pub path: String,
    pub network: Network,
    pub ldk_conf: UserConfig,
    pub port: u64,
}
