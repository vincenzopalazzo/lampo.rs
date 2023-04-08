mod args;

use clap::Parser;
use clightningrpc_conf::SyncCLNConf;
use lampod::conf::{LampoConf, UserConfig};
use lampod::LampoDeamon;

use crate::args::LampoCliArgs;

#[tokio::main]
async fn main() -> Result<(), ()> {
    let args = LampoCliArgs::parse();
    run(args).await?;
    Ok(())
}

async fn run(args: LampoCliArgs) -> Result<(), ()> {
    let Ok(conf) = clightningrpc_conf::CLNConf::new(args.conf.clone().unwrap(), false)
        .parse() else {
            return Err(());
        };

    let lampo_conf = LampoConf {
        network: lampod::chain::Network::Testnet,
        port: 9768,
        path: args.conf.unwrap(),
        ldk_conf: UserConfig::default(),
    };
    let lampod = LampoDeamon::new(lampo_conf);
    // FIXME: implement these
    //lampod.init(client, keys);
    lampod.listen().await?;
    Ok(())
}
