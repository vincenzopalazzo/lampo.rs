// use clap::Parser;
use clap::{Parser, Subcommand};

use lampo_common::conf::{LampoConf, Network};
use lampo_common::error;

#[derive(Subcommand, Debug, Clone)]
pub enum LampoCliSubcommand {
    /// Create a new wallet and print the mnemonic
    NewWallet,
}

#[derive(Parser, Debug, Clone)]
#[command(
    name = "lampod-cli",
    about = "Lampo Daemon command line",
    version = env!("CARGO_PKG_VERSION"),
    long_about = None
)]
pub struct LampoCliArgs {
    /// Override the default path of the config field
    #[arg(short = 'd', long = "data-dir")]
    pub data_dir: Option<String>,

    /// Set the network for lampo
    #[arg(short = 'n', long = "network")]
    pub network: Option<String>,

    /// Set the default lampo bitcoin backend
    #[arg(long = "client")]
    pub client: Option<String>,

    /// Restore a wallet from a mnemonic
    #[arg(long = "restore-wallet")]
    pub restore_wallet: bool,

    /// Set the log level, by default is `info`
    #[arg(long = "log-level")]
    pub log_level: Option<String>,

    /// Redirect the lampo logs on the file
    #[arg(long = "log-file")]
    pub log_file: Option<String>,

    /// Set the url of the bitcoin core backend
    #[arg(long = "core-url")]
    pub bitcoind_url: Option<String>,

    /// Set the username of the bitcoin core backend
    #[arg(long = "core-user")]
    pub bitcoind_user: Option<String>,

    /// Set the password of the bitcoin core backend
    #[arg(long = "core-pass")]
    pub bitcoind_pass: Option<String>,

    /// Force polling in development mode
    #[arg(long = "dev-force-poll", hide = true)]
    pub dev_force_poll: bool,

    /// Set the API host
    #[arg(long = "api-host")]
    pub api_host: Option<String>,

    /// Set the API port
    #[arg(long = "api-port")]
    pub api_port: Option<u64>,

    /// Subcommand to run
    #[command(subcommand)]
    pub subcommand: Option<LampoCliSubcommand>,
}

impl TryInto<LampoConf> for LampoCliArgs {
    type Error = error::Error;

    fn try_into(self) -> Result<LampoConf, Self::Error> {
        let mut conf = LampoConf::default();

        // if network is not specified, set the testnet dir
        let network = self.network.unwrap_or(String::from("testnet"));
        conf.network = match network.as_str() {
            "bitcoin" => Network::Bitcoin,
            "testnet" => Network::Testnet,
            "regtest" => Network::Regtest,
            "signet" => Network::Signet,
            _ => error::bail!("Invalid network {network}"),
        };

        let path = self.data_dir.unwrap_or(conf.root_path);
        // FIXME: this override the full configuration, we should merge the two
        conf = LampoConf::new(Some(path), Some(conf.network), None)?;
        conf.prepare_dirs()?;

        log::debug!(target: "lampod-cli", "lampo data dir `{}`", conf.path());
        log::debug!(target: "lampod-cli", "client from args {:?}", self.client);
        // Override the lampo conf with the args from the cli
        if let Some(node) = self.client {
            conf.node = node.clone();
        }
        if self.bitcoind_url.is_some() {
            conf.core_url = self.bitcoind_url;
        }
        if self.bitcoind_user.is_some() {
            conf.core_user = self.bitcoind_user;
        }
        if self.bitcoind_pass.is_some() {
            conf.core_pass = self.bitcoind_pass;
        }
        if self.log_file.is_some() {
            conf.log_file = self.log_file;
        }
        if self.log_level.is_some() {
            conf.log_level = self.log_level.unwrap();
        }
        if let Some(api_host) = self.api_host {
            conf.api_host = api_host;
        }
        if let Some(api_port) = self.api_port {
            conf.api_port = api_port;
        }
        Ok(conf)
    }
}

pub fn parse_args() -> Result<LampoCliArgs, error::Error> {
    Ok(LampoCliArgs::parse())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_with_data_dir(data_dir: &str) -> LampoCliArgs {
        LampoCliArgs {
            data_dir: Some(data_dir.to_string()),
            network: Some("regtest".to_string()),
            client: None,
            restore_wallet: false,
            log_level: None,
            log_file: None,
            bitcoind_url: None,
            bitcoind_user: None,
            bitcoind_pass: None,
            dev_force_poll: false,
            api_host: None,
            api_port: None,
            subcommand: None,
        }
    }

    /// REPRO (bug 3, CLI side): `try_into` calls `LampoConf::default()`
    /// before overwriting the conf with `LampoConf::new`, so the panic on
    /// an undeterminable home directory triggers even when the user
    /// explicitly passes `--data-dir`.
    ///
    /// NOTE: ignored on normal developer machines, see
    /// `lampo_common::conf::tests::lampo_conf_default_panics_without_home`.
    #[test]
    #[ignore = "only panics where no passwd fallback exists (containers)"]
    #[should_panic(expected = "Impossible to get the home directory")]
    fn try_into_panics_without_home_even_with_data_dir() {
        std::env::remove_var("HOME");
        let args = args_with_data_dir("/tmp/lampo-cli-repro-no-home");
        let _: LampoConf = args.try_into().unwrap();
    }
}
