mod args;

use std::process::exit;

use log;
use radicle_term as term;

use lampo_client::UnixClient;
use lampo_common::error;
use lampo_common::json;
use lampo_common::logger;

use crate::args::LampoCliArgs;

fn main() -> error::Result<()> {
    logger::init(log::Level::Info).unwrap();
    let args = match args::parse_args() {
        Ok(args) => args,
        Err(err) => {
            term::error(format!("{err}"));
            exit(1);
        }
    };
    let resp = run(args)?;
    println!("{}", term::format::bold(json::to_string_pretty(&resp)?));
    Ok(())
}

fn run(args: LampoCliArgs) -> error::Result<json::Value> {
    let client = UnixClient::new(&args.socket).unwrap();
    let resp = client.call(&args.method, args.args);
    Ok(resp.map_err(|err| error::anyhow!("{:?}", err))?)
}
