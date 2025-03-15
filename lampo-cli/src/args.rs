use std::collections::HashMap;

use radicle_term as term;

use lampo_common::error;
use lampo_common::json;

#[derive(Debug)]
pub struct LampoCliArgs {
    pub method: String,
    pub args: HashMap<String, json::Value>,
    pub url: String,
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
    -u | --url        Specify API endipoint
    -h | --help         Print help
"#,
};

pub fn parse_args() -> Result<LampoCliArgs, lexopt::Error> {
    use lexopt::prelude::*;

    let mut _network: Option<String> = None;
    let mut method: Option<String> = None;
    let mut url: Option<String> = None;
    let mut args = HashMap::<String, json::Value>::new();

    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next()? {
        match arg {
            Short('n') | Long("network") => {
                let val: String = parser.value()?.parse()?;
                _network = Some(val);
            }
            Short('u') | Long("url") => {
                let var: String = parser.value()?.parse()?;
                url = Some(var);
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

    log::debug!("args parser are {:?} {:?}", method, args);
    Ok(LampoCliArgs {
        url: url.unwrap_or("http://127.0.0.1:7979".to_string()),
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
