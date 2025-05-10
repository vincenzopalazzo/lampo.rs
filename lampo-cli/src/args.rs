use std::collections::HashMap;

use clap::Parser;

use lampo_common::error;
use lampo_common::json;

#[derive(Debug, Parser)]
#[command(name = "lampod-cli")]
#[command(about = "Lampo Daemon command line", version = env!("CARGO_PKG_VERSION"))]
#[command(before_help = "Common `lampod-cli` commands used to interact with the lampo daemon")]
#[command(after_help = "Example: lampod-cli get_block height=123 verbose=true")]
pub struct LampoCliArgs {
    /// RPC method to call
    #[arg(value_parser)]
    pub method: String,

    /// Network for lampo
    #[arg(short, long, default_value = "testnet")]
    pub network: String,

    /// API endpoint URL
    #[arg(short, long, default_value = "http://127.0.0.1:7979")]
    pub url: String,

    /// Method arguments in key=value format
    #[arg(value_parser = parse_key_val, value_name = "KEY=VALUE")]
    pub args: Vec<(String, json::Value)>,
}

impl LampoCliArgs {
    /// Convert the Vec of key-value pairs to a HashMap
    pub fn args_as_hashmap(&self) -> HashMap<String, json::Value> {
        let mut map = HashMap::new();

        // For duplicate keys, the last value wins
        for (key, value) in &self.args {
            map.insert(key.clone(), value.clone());
        }

        map
    }
}

/// Parse a key-value pair in the format key=value
fn parse_key_val(s: &str) -> Result<(String, json::Value), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{s}`"))?;
    let key = s[..pos].to_string();
    let val = s[pos + 1..].to_string();

    // Try to parse as different types
    if let Ok(val) = val.parse::<u64>() {
        Ok((key, json::json!(val)))
    } else if let Ok(val) = val.parse::<bool>() {
        Ok((key, json::json!(val)))
    } else {
        Ok((key, json::json!(val)))
    }
}

/// Parse command line arguments into LampoCliArgs
pub fn parse_args() -> error::Result<LampoCliArgs> {
    Ok(LampoCliArgs::parse())
}
