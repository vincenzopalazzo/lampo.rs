mod args;

use lampo_client::errors::Error;
use lampo_client::UnixClient;
use lampo_common::error;
use lampo_common::json;
use radicle_term as term;
use std::process::exit;

use crate::args::{parse_args, LampoCliArgs};

fn main() -> error::Result<()> {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            term::error(format!("Error parsing arguments: {}", err));
            exit(1);
        }
    };

    let resp = run(args);
    match resp {
        Ok(resp) => {
            term::print(json::to_string_pretty(&resp)?);
        }
        Err(Error::Rpc(rpc)) => {
            term::print(json::to_string_pretty(&rpc)?);
        }
        Err(err) => {
            term::error(format!("{err}"));
        }
    }
    Ok(())
}

fn run(args: LampoCliArgs) -> Result<json::Value, lampo_client::errors::Error> {
    let client = UnixClient::new(&args.socket).unwrap();
    let resp = client.call(&args.method, args.args)?;
    Ok(resp)
}
