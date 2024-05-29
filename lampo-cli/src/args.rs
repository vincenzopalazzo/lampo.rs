use std::collections::HashMap;

use radicle_term as term;

use lampo_common::error;
use lampo_common::json;

#[derive(Debug)]
pub struct LampoCliArgs {
    pub socket: String,
    pub method: String,
    pub args: HashMap<String, json::Value>,
}

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

    lampod-cli [<option> ...] <method> [arg=value]

Options

    -d | --data-dir     Specify lampo data directory (used to get socket path)
    -n | --network      Set the network for lampo (default: testnet)
    -s | --socket       Specify Unix Socket patch of the lampod node directely
    -h | --help         Print help
"#,
};

pub fn parse_args() -> Result<LampoCliArgs, lexopt::Error> {
    use lexopt::prelude::*;

    let mut data_dir: Option<String> = None;
    let mut network: Option<String> = None;
    let mut socket: Option<String> = None;
    let mut method: Option<String> = None;
    let mut args = HashMap::<String, json::Value>::new();

    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next()? {
        match arg {
            Short('d') | Long("data-dir") => {
                let val: String = parser.value()?.parse()?;
                data_dir = Some(val);
            }
            Short('n') | Long("network") => {
                let val: String = parser.value()?.parse()?;
                network = Some(val);
            }
            Short('s') | Long("socket") => {
                let val: String = parser.value()?.parse()?;
                socket = Some(val);
            }
            Long("help") => {
                let _ = print_help();
                std::process::exit(0);
            }
            Long(val) => {
                if method.is_none() {
                    return Err(lexopt::Error::MissingValue {
                        option: Some("method is not specified".to_owned()),
                    });
                }
                log::debug!("look for args {:?}", val);
                match arg {
                    Long(val) => {
                        let key = val.to_string();
                        let val: String = parser.value()?.parse()?;
                        if let Ok(val) = val.parse::<u64>() {
                            let val = json::json!(val);
                            args.insert(key.clone(), val);
                        } else if let Ok(val) = val.parse::<bool>() {
                            let val = json::json!(val);
                            args.insert(key.clone(), val);
                        } else {
                            let val = json::json!(val);
                            args.insert(key, val);
                        }
                    }
                    _ => return Err(arg.unexpected()),
                }
            }
            Value(ref val) => {
                if args.is_empty() && method.is_none() {
                    method = Some(val.clone().string()?);
                    log::debug!("find a method {:?}", method);
                    continue;
                }
                return Err(arg.unexpected());
            }
            _ => return Err(arg.unexpected()),
        }
    }

    // If data-dir is specified and socket is not specified,
    // we need to get the socket path from it
    // by appending the network name (default: testnet) to the path
    // and adding the socket path (lampod.socket)
    if socket.is_none() {
        let data_dir = data_dir
            .or_else(|| {
                #[allow(deprecated)]
                std::env::home_dir().and_then(|path| Some(path.to_string_lossy().to_string()))
            })
            .unwrap();
        let data_dir = format!("{data_dir}/.lampo");
        let network = network.unwrap_or_else(|| "testnet".to_owned());
        let socket_path = format!("{}/{}{}", data_dir, network, "/lampod.socket");
        log::debug!("socket path is {:?}", socket_path);
        socket = Some(socket_path);
    }

    log::debug!("args parser are {:?} {:?}", method, args);
    Ok(LampoCliArgs {
        socket: socket.ok_or_else(|| lexopt::Error::MissingValue {
            option: Some("Socket path need to be specified".to_owned()),
        })?,
        method: method.ok_or_else(|| lexopt::Error::MissingValue {
            option: Some(
                "Too few params, a method need to be specified. Try run `lampo-cli --help`"
                    .to_owned(),
            ),
        })?,
        args,
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
        term::format::dim("lampod-cli [<option> ...] <method> [arg=value]")
    );
    println!();

    println!(
        "\t{} version {}",
        term::format::bold("lampo-cli"),
        term::format::dim(HELP.version)
    );
    println!(
        "\t{} {}",
        term::format::bold(format!("{:-12}", HELP.name)),
        term::format::dim(HELP.description)
    );
    println!("{}", term::format::bold(HELP.usage));
    Ok(())
}
