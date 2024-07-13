mod args;

use std::process::exit;

use radicle_term as term;

use lampo_client::LampoClient;
use lampo_common::error;
use lampo_common::json;

use crate::args::LampoCliArgs;

#[tokio::main]
async fn main() -> error::Result<()> {
    let args = match args::parse_args() {
        Ok(args) => args,
        Err(err) => {
            term::error(format!("{err}"));
            exit(1);
        }
    };
    let resp = run(args).await;
    match resp {
        Ok(resp) => {
            term::print(json::to_string_pretty(&resp)?);
        }
        Err(err) => {
            term::error(format!("{err}"));
        }
    }
    Ok(())
}

async fn run(args: LampoCliArgs) -> error::Result<json::Value> {
    let client = LampoClient::new(&args.socket).await?;
    let resp = client.call(&args.method, args.args).await?;
    Ok(resp)
}
