use radicle_term as term;

use lampo_common::error;

struct Help {
    name: &'static str,
    description: &'static str,
    version: &'static str,
    usage: &'static str,
}

const HELP: Help = Help {
    name: "lampod-cli",
    description: "Lampo Deamon command line",
    version: env!("CARGO_PKG_VERSION"),
    usage: r#"
Usage

    lampod-cli [<option> ...]

Options

    -c | --config    Override the default path of the config field
    -n | --network   Set the network for lampo
    -h | --help      Print help
    --client         Set the default lampo bitcoin backend
"#,
};

#[derive(Debug)]
pub struct LampoCliArgs {
    pub conf: String,
    pub network: Option<String>,
    pub client: Option<String>,
    pub mnemonic: Option<String>,
    pub bitcoind_url: Option<String>,
    pub bitcoind_user: Option<String>,
    pub bitcoind_pass: Option<String>,
}

pub fn parse_args() -> Result<LampoCliArgs, lexopt::Error> {
    use lexopt::prelude::*;

    let mut config: Option<String> = None;
    let mut network: Option<String> = None;
    let mut client: Option<String> = None;
    let mut bitcoind_url: Option<String> = None;
    let mut bitcoind_user: Option<String> = None;
    let mut bitcoind_pass: Option<String> = None;
    let mut mnemonic: Option<String> = None;

    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next()? {
        match arg {
            Short('c') | Long("config") => {
                let val: String = parser.value()?.parse()?;
                config = Some(val);
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
                let var: String = parser.value()?.parse()?;
                mnemonic = Some(var);
            }
            Long("help") => {
                let _ = print_help();
                std::process::exit(0);
            }
            _ => return Err(arg.unexpected()),
        }
    }

    Ok(LampoCliArgs {
        conf: config.ok_or_else(|| lexopt::Error::MissingValue {
            option: Some("config is not specified".to_owned()),
        })?,
        network,
        client,
        mnemonic,
        bitcoind_url,
        bitcoind_pass,
        bitcoind_user,
    })
}

// Print helps
pub fn print_help() -> error::Result<()> {
    println!(
        "{}",
        term::format::secondary("Common `lampod-cli` commands used to init the lampo deamon")
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
