mod args;
mod logger;

use std::str::FromStr;
use std::sync::Arc;

use clap::Parser;
use clightningrpc_conf::SyncCLNConf;
use lampo_nakamoto::{Config, Nakamoto, Network};
use lampod::conf::{LampoConf, UserConfig};
use lampod::keys::keys::LampoKeys;
use lampod::LampoDeamon;

use crate::args::LampoCliArgs;

#[tokio::main]
async fn main() -> Result<(), ()> {
    logger::init(log::Level::Debug).expect("initializing logger for the first time");
    let args = LampoCliArgs::parse();
    run(args).await?;
    Ok(())
}

async fn run(args: LampoCliArgs) -> Result<(), ()> {
    let Ok(conf) = clightningrpc_conf::CLNConf::new(args.conf.clone().unwrap(), false)
        .parse() else {
            return Err(());
        };

    let network_str = args.network.unwrap();
    let network = match network_str.as_str() {
        "bitcoin" => Network::Mainnet,
        "testnet" => Network::Testnet,
        _ => panic!("unsupported network {network_str}"),
    };

    let lampo_conf = LampoConf {
        network: lampod::conf::Network::from_str(&network_str).unwrap(),
        port: 9768,
        path: args.conf.unwrap(),
        ldk_conf: UserConfig::default(),
    };
    let mut lampod = LampoDeamon::new(lampo_conf);
    let keys = Arc::new(LampoKeys::new());
    let client = match args.client.clone().unwrap().as_str() {
        "nakamoto" => {
            let mut conf = Config::default();
            conf.network = network;
            Arc::new(Nakamoto::new(conf).unwrap())
        }
        _ => panic!("client {:?} not supported", args.client),
    };
    lampod.init(client, keys).await?;
    lampod.listen().await?;
    Ok(())
}
