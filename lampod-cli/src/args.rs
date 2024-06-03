use radicle_term as term;

use lampo_common::conf::{LampoConf, Network};
use lampo_common::error;

struct Help {
    name: &'static str,
    description: &'static str,
    version: &'static str,
    usage: &'static str,
}

const HELP: Help = Help {
    name: "lampod-cli",
    description: "Lampo Daemon command line",
    version: env!("CARGO_PKG_VERSION"),
    usage: r#"
Usage

    lampod-cli [<option> ...]

Options

    -d | --data-dir    Override the default path of the config field
    -n | --network     Set the network for lampo
    -h | --help        Print help

    --log-file         Redirect the lampo logs on the file
    --log-level        Set the log level, by default is `info`
    --client           Set the default lampo bitcoin backend
    --core-url         Set the url of the bitcoin core backend
    --core-user        Set the username of the bitcoin core backend
    --core-pass        Set the password of the bitcoin core backend
    --restore-wallet   Restore a wallet from a mnemonic 
"#,
};

#[derive(Debug)]
pub struct LampoCliArgs {
    pub data_dir: Option<String>,
    pub network: Option<String>,
    pub client: Option<String>,
    pub restore_wallet: bool,
    pub log_level: Option<String>,
    pub log_file: Option<String>,
    pub bitcoind_url: Option<String>,
    pub bitcoind_user: Option<String>,
    pub bitcoind_pass: Option<String>,
    pub dev_force_poll: bool,
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
        Ok(conf)
    }
}

pub fn parse_args() -> Result<LampoCliArgs, lexopt::Error> {
    use lexopt::prelude::*;

    let mut data_dir: Option<String> = None;
    let mut log_file: Option<String> = None;
    let mut level: Option<String> = None;
    let mut network: Option<String> = None;
    let mut client: Option<String> = None;
    let mut bitcoind_url: Option<String> = None;
    let mut bitcoind_user: Option<String> = None;
    let mut bitcoind_pass: Option<String> = None;
    let mut restore_wallet = false;
    let mut dev_force_poll = false;

    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next()? {
        match arg {
            Short('d') | Long("data-dir") => {
                let val: String = parser.value()?.parse()?;
                data_dir = Some(val);
            }
            Long("log-file") => {
                let val: String = parser.value()?.parse()?;
                log_file = Some(val);
            }
            Long("log-level") => {
                let val: String = parser.value()?.parse()?;
                level = Some(val);
            }
            Short('n') | Long("network") => {
                let val: String = parser.value()?.parse()?;
                network = Some(val);
            }
            Long("client") => {
                let var: String = parser.value()?.parse()?;
                client = Some(var);
            }
            Long("core-url") => {
                let var: String = parser.value()?.parse()?;
                bitcoind_url = Some(var);
            }
            Long("core-user") => {
                let var: String = parser.value()?.parse()?;
                bitcoind_user = Some(var);
            }
            Long("core-pass") => {
                let var: String = parser.value()?.parse()?;
                bitcoind_pass = Some(var);
            }
            Long("restore-wallet") => {
                restore_wallet = true;
            }
            // FIXME: allow only in debug mode
            Long("dev-force-poll") => dev_force_poll = true,
            Long("help") => {
                let _ = print_help();
                std::process::exit(0);
            }
            _ => return Err(arg.unexpected()),
        }
    }

    Ok(LampoCliArgs {
        data_dir,
        network,
        client,
        restore_wallet,
        log_file,
        bitcoind_url,
        bitcoind_pass,
        bitcoind_user,
        dev_force_poll,
        // Default log level is info if it is not specified
        // in the command line
        log_level: level,
    })
}

// Print helps
pub fn print_help() -> error::Result<()> {
    println!(
        "{}",
        term::format::secondary("Common `lampod-cli` commands used to init the lampo daemon")
    );
    println!(
        "\n{} {}",
        term::format::bold("Usage:"),
        term::format::dim("lampod-cli <command> [--help]")
    );
    println!();

    println!(
        "\t{} {}",
        term::format::bold(format!("{:-12}", HELP.name)),
        term::format::dim(HELP.description)
    );
    println!("{}", term::format::bold(HELP.usage));
    Ok(())
}
