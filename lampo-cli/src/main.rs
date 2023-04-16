mod args;
mod logger;

use std::str::FromStr;
use std::sync::Arc;

use anyhow;
use clap::Parser;

use lampo_nakamoto::{Config, Nakamoto, Network};
use lampod::conf::{LampoConf, UserConfig};
use lampod::keys::keys::LampoKeys;
use lampod::LampoDeamon;

use crate::args::LampoCliArgs;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    logger::init(log::Level::Debug).expect("initializing logger for the first time");
    let args = LampoCliArgs::parse();
    run(args).await?;
    Ok(())
}

async fn run(args: LampoCliArgs) -> anyhow::Result<()> {
    let Some(path) = args.conf else {
            anyhow::bail!("Fails to parse the conf file at path {:?}", args.conf);
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
        _ => anyhow::bail!("client {:?} not supported", args.client),
    };
    lampod.init(client, keys).await?;
    lampod.listen().await?;
    Ok(())
}
