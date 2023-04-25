use std::collections::HashMap;

use radicle_term as term;

use lampo_common::error;

#[derive(Debug)]
pub struct LampoCliArgs {
    pub socket: String,
    pub method: String,
    pub args: HashMap<String, String>,
}

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

    lampod-cli [<option> ...] <method> [arg=value]

Options

    -s | --socket    Unix Socket patch of the lampod node
    -h | --help      Print help
"#,
};

pub fn parse_args() -> Result<LampoCliArgs, lexopt::Error> {
    use lexopt::prelude::*;

    let mut socket: Option<String> = None;
    let mut method: Option<String> = None;
    let mut args = HashMap::<String, String>::new();

    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next()? {
        match arg {
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
                        let val = parser.value()?.parse()?;
                        args.insert(key, val);
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
        socket: socket.expect("Socket path need to be specified"),
        method: method
            .expect("Too few params, a method need to be specified. Try run `lampo-cli --help`"),
        args,
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
