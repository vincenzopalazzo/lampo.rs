mod args;

use args::LampoCommands;
use clap::Parser;
use lampo_client::UnixClient;
use log;

use lampo_common::{
    json::{self, error},
    logger,
    model::Connect,
};

use crate::args::LampoCliArgs;

fn main() -> error::Result<()> {
    logger::init(log::Level::Info).unwrap();
    let args = LampoCliArgs::parse();
    let resp = run(args)?;
    println!("{}", json::to_string_pretty(&resp)?);
    Ok(())
}

fn run(args: LampoCliArgs) -> error::Result<json::Value> {
    let client = UnixClient::new(&args.socket).unwrap();
    match args.method {
        LampoCommands::Connect {
            node_id,
            addr,
            port,
        } => {
            let input = Connect {
                node_id,
                addr,
                port,
            };
            let resp: json::Value = client.call("connect", input).unwrap();
            Ok(resp)
        }
        LampoCommands::GetInfo => {
            let resp: json::Value = client.call("getinfo", json::json!({})).unwrap();
            Ok(resp)
        }
    }
}
