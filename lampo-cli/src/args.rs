use std::collections::HashMap;
use std::ffi::OsString;

use clap::{CommandFactory, Parser, Subcommand};

use lampo_common::json;

#[derive(Subcommand, Debug)]
enum CliCommand {
    #[command(external_subcommand)]
    Method(Vec<OsString>),
}

#[derive(Parser, Debug)]
#[command(
    name = "lampo-cli",
    about = "Lampo Daemon command line",
    version = env!("CARGO_PKG_VERSION"),
    long_about = None
)]
struct CliParser {
    /// Specify API endpoint
    #[arg(short = 'u', long = "url")]
    pub url: Option<String>,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug)]
pub struct LampoCliArgs {
    pub method: String,
    pub args: HashMap<String, json::Value>,
    pub url: String,
}

/// Parse a value string into the appropriate JSON type.
/// Tries u64 first, then bool, then falls back to string.
fn parse_value(val: &str) -> json::Value {
    if let Ok(n) = val.parse::<u64>() {
        json::json!(n)
    } else if let Ok(b) = val.parse::<bool>() {
        json::json!(b)
    } else {
        json::json!(val)
    }
}

pub fn parse_args() -> Result<LampoCliArgs, clap::Error> {
    let cli = CliParser::try_parse()?;

    let url = cli
        .url
        .unwrap_or_else(|| "http://127.0.0.1:7979".to_string());

    let ext_args = match cli.command {
        Some(CliCommand::Method(args)) => args,
        None => {
            return Err(CliParser::command().error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "A method must be specified. Try `lampo-cli --help`",
            ));
        }
    };

    // First element is the method name
    let method = ext_args
        .first()
        .expect("external subcommand always has at least one element")
        .to_string_lossy()
        .to_string();

    // Parse remaining --key value pairs into a HashMap
    let trailing: Vec<String> = ext_args[1..]
        .iter()
        .map(|s| s.to_string_lossy().to_string())
        .collect();

    let mut args = HashMap::<String, json::Value>::new();
    let mut i = 0;
    while i < trailing.len() {
        let arg = &trailing[i];
        if let Some(key) = arg.strip_prefix("--") {
            i += 1;
            if i >= trailing.len() {
                return Err(CliParser::command().error(
                    clap::error::ErrorKind::MissingRequiredArgument,
                    format!("missing value for argument '--{key}'"),
                ));
            }
            let val = &trailing[i];
            log::debug!("look for args {:?} = {:?}", key, val);
            args.insert(key.to_string(), parse_value(val));
        } else {
            return Err(CliParser::command().error(
                clap::error::ErrorKind::InvalidValue,
                format!("unexpected positional argument '{arg}'"),
            ));
        }
        i += 1;
    }

    log::debug!("args parser are {:?} {:?}", method, args);
    Ok(LampoCliArgs { url, method, args })
}
