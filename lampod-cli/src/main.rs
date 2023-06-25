#[allow(dead_code)]
mod args;

use std::env;
use std::io;
use std::str::FromStr;
use std::sync::Arc;
use std::thread::JoinHandle;

use lampod::jsonrpc::channels::json_list_channels;
use log;

use lampo_bitcoind::BitcoinCore;
use lampo_common::backend::Backend;
use lampo_common::bitcoin;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::handler::Handler as _;
use lampo_common::logger;
use lampo_common::secp256k1;
use lampo_jsonrpc::Handler;
use lampo_jsonrpc::JSONRPCv2;
use lampo_nakamoto::{Config, Nakamoto, Network};
use lampod::chain::{LampoWalletManager, WalletManager};
use lampod::jsonrpc::inventory::get_info;
use lampod::jsonrpc::onchain::json_funds;
use lampod::jsonrpc::onchain::json_new_addr;
use lampod::jsonrpc::open_channel::json_open_channel;
use lampod::jsonrpc::peer_control::json_connect;
use lampod::jsonrpc::CommandHandler;
use lampod::LampoDeamon;

use crate::args::LampoCliArgs;

fn main() -> error::Result<()> {
    logger::init(log::Level::Info).expect("unable to init the logger for the first time");
    let args = args::parse_args()?;
    run(args)?;
    Ok(())
}

/// Return the root directory.
fn run(args: LampoCliArgs) -> error::Result<()> {
    let path = args.conf;
    let mut lampo_conf = LampoConf::try_from(path)?;
    if args.network.is_some() {
        lampo_conf.set_network(&args.network.unwrap())?;
    }

    let wallet = if let Some(ref private_key) = lampo_conf.private_key {
        #[cfg(debug_assertions)]
        {
            let key = secp256k1::SecretKey::from_str(&private_key)?;
            let key = bitcoin::PrivateKey::new(key, lampo_conf.network);
            LampoWalletManager::try_from((key, None))?
        }
        #[cfg(not(debug_assertions))]
        unimplemented!()
    } else {
        if args.mnemonic.is_none() {
            let (wallet, mnemonic) = LampoWalletManager::new(Arc::new(lampo_conf.clone()))?;
            radicle_term::success!("Wallet Generated, please store this works in a safe way");
            radicle_term::println(
                radicle_term::format::badge_primary("waller-keys"),
                format!("{}", radicle_term::format::highlight(mnemonic)),
            );
            wallet
        } else {
            // SAFETY: It is safe to unwrap the mnemonic because we check it
            // before.
            LampoWalletManager::restore(Arc::new(lampo_conf.clone()), &args.mnemonic.unwrap())?
        }
    };
    let mut lampod = LampoDeamon::new(lampo_conf.clone(), Arc::new(wallet));
    let client = args.client.unwrap_or(lampo_conf.node.clone());
    let client: Arc<dyn Backend> = match client.as_str() {
        "nakamoto" => {
            let mut conf = Config::default();
            conf.network = Network::from_str(&lampo_conf.network.to_string()).unwrap();
            Arc::new(Nakamoto::new(conf).unwrap())
        }
        "core" => Arc::new(BitcoinCore::new(
            &args
                .bitcoind_url
                .unwrap_or(lampo_conf.core_url.clone().unwrap()),
            &args
                .bitcoind_user
                .unwrap_or(lampo_conf.core_user.clone().unwrap()),
            &args
                .bitcoind_pass
                .unwrap_or(lampo_conf.core_pass.clone().unwrap()),
        )?),
        _ => error::bail!("client {:?} not supported", client),
    };
    lampod.init(client)?;

    let rpc_handler = Arc::new(CommandHandler::new(&lampo_conf)?);
    lampod.add_external_handler(rpc_handler.clone())?;

    let mut _pid = filelock_rs::pid::Pid::new(lampo_conf.path, "lampod".to_owned())
        .map_err(|_| error::anyhow!("impossible take a lock on the `lampod.pid` file, maybe there is another instance running?"))?;

    let lampod = Arc::new(lampod);
    let (jsorpc_worker, handler) = run_jsonrpc(lampod.clone()).unwrap();
    rpc_handler.set_handler(handler.clone());

    ctrlc::set_handler(move || {
        use std::time::Duration;
        log::info!("Shutdown...");
        handler.stop();
        std::thread::sleep(Duration::from_secs(5));
        std::process::exit(0);
    })?;
    let handler = lampod.handler();

    // Just as debugging for us to manage the event through by lampod.
    std::thread::spawn(move || {
        while let Ok(event) = handler.events().recv() {
            log::info!(target: "lampod-cli", "event emitted `{:?}`", event);
        }
    });

    let workder = lampod.listen().unwrap();
    let _ = workder.join();
    let _ = jsorpc_worker.join().unwrap();
    Ok(())
}

fn run_jsonrpc(
    lampod: Arc<LampoDeamon>,
) -> error::Result<(JoinHandle<io::Result<()>>, Arc<Handler<LampoDeamon>>)> {
    let socket_path = format!("{}/lampod.socket", lampod.root_path());
    // we take the lock with the pid file so if we are at this point
    // we can delete the socket because there is no other process
    // that it is running.
    let _ = std::fs::remove_file(socket_path.clone());
    env::set_var("LAMPO_UNIX", socket_path.clone());
    let server = JSONRPCv2::new(lampod, &socket_path)?;
    server.add_rpc("getinfo", get_info).unwrap();
    server.add_rpc("connect", json_connect).unwrap();
    server.add_rpc("fundchannel", json_open_channel).unwrap();
    server.add_rpc("newaddr", json_new_addr).unwrap();
    server.add_rpc("channels", json_list_channels).unwrap();
    server.add_rpc("funds", json_funds).unwrap();
    let handler = server.handler();
    Ok((server.spawn(), handler))
}
