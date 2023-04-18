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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::{net::SocketAddr, str::FromStr};

    use clightningrpc_conf::CLNConf;
    use lightning::util::config::UserConfig;

    use lampo_nakamoto::{Config, Network};
    use lampod::{
        conf::LampoConf,
        keys::keys::LampoKeys,
        ln::events::{NodeId, PeerEvents},
        LampoDeamon,
    };

    use crate::logger;

    #[tokio::test]
    async fn simple_node_connection() {
        logger::init(log::Level::Debug).expect("initializing logger for the first time");
        let conf = LampoConf {
            ldk_conf: UserConfig::default(),
            network: bitcoin::Network::Testnet,
            port: 19753,
            path: "/tmp".to_string(),
            inner: CLNConf::new("/tmp/".to_owned(), true),
        };
        let mut lampo = LampoDeamon::new(conf);

        let mut conf = Config::default();
        conf.network = Network::Testnet;
        let client = Arc::new(lampo_nakamoto::Nakamoto::new(conf).unwrap());

        let result = lampo.init(client, Arc::new(LampoKeys::new())).await;
        assert!(result.is_ok());

        let node_id =
            NodeId::from_str("02049b60c296ffead3e7c8b124c5730153403a8314c1116c2d1b43cf9ac0de2d9d")
                .unwrap();
        let addr = SocketAddr::from_str("78.46.220.4:19735").unwrap();
        let result = lampo.peer_manager().connect(node_id, addr).await;
        assert!(result.is_ok(), "{:?}", result);
    }
}
