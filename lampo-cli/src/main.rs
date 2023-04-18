mod args;

use std::str::FromStr;
use std::sync::Arc;

use clap::Parser;
use log;

use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::json;
use lampo_common::logger;
use lampo_nakamoto::{Config, Nakamoto, Network};
use lampod::keys::keys::LampoKeys;
use lampod::LampoDeamon;

use crate::args::LampoCliArgs;

#[tokio::main]
async fn main() -> error::Result<()> {
    logger::init(log::Level::Debug).expect("initializing logger for the first time");
    let args = LampoCliArgs::parse();
    run(args).await?;
    Ok(())
}

async fn run(args: LampoCliArgs) -> error::Result<()> {
    let Some(path) = args.conf else {
            error::bail!("Fails to parse the conf file at path {:?}", args.conf);
    };
    let mut lampo_conf = LampoConf::try_from(path)?;

    if let Some(network_str) = args.network {
        lampo_conf.set_network(&network_str)?;
    };

    let mut lampod = LampoDeamon::new(lampo_conf.clone());
    let keys = Arc::new(LampoKeys::new());
    let client = match args.client.clone().unwrap().as_str() {
        "nakamoto" => {
            let mut conf = Config::default();
            conf.network = Network::from_str(&lampo_conf.network.to_string()).unwrap();
            Arc::new(Nakamoto::new(conf).unwrap())
        }
        _ => error::bail!("client {:?} not supported", args.client),
    };
    lampod.init(client, keys).await?;
    let info = lampod.get_info()?;
    log::info!("node info: {}", json::to_string_pretty(&info)?);
    lampod.listen().await?;
    Ok(())
}
