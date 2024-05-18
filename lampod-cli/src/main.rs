#[allow(dead_code)]
mod args;

use std::env;
use std::io;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::thread::JoinHandle;

use radicle_term as term;

use lampo_bitcoind::BitcoinCore;
use lampo_common::backend::Backend;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::logger;
use lampo_core_wallet::CoreWalletManager;
use lampo_jsonrpc::Handler;
use lampo_jsonrpc::JSONRPCv2;
use lampod::chain::WalletManager;
use lampod::jsonrpc::channels::json_close_channel;
use lampod::jsonrpc::channels::json_list_channels;
use lampod::jsonrpc::inventory::get_info;
use lampod::jsonrpc::offchain::json_decode_invoice;
use lampod::jsonrpc::offchain::json_invoice;
use lampod::jsonrpc::offchain::json_keysend;
use lampod::jsonrpc::offchain::json_offer;
use lampod::jsonrpc::offchain::json_pay;
use lampod::jsonrpc::onchain::json_estimate_fees;
use lampod::jsonrpc::onchain::json_funds;
use lampod::jsonrpc::onchain::json_new_addr;
use lampod::jsonrpc::open_channel::json_open_channel;
use lampod::jsonrpc::peer_control::json_connect;
use lampod::jsonrpc::CommandHandler;
use lampod::LampoDeamon;

use crate::args::LampoCliArgs;

fn main() -> error::Result<()> {
    log::debug!("Started!");
    let args = args::parse_args()?;
    run(args)?;
    Ok(())
}

/// Return the root directory.
fn run(args: LampoCliArgs) -> error::Result<()> {
    let mnemonic = if args.restore_wallet {
        let inputs: String = term::input(
            "BIP 39 Mnemonic",
            None,
            Some("To restore the wallet, lampo needs a BIP39 mnemonic with words separated by spaces."),
        )?;
        Some(inputs)
    } else {
        None
    };

    // After this point the configuration is ready!
    let mut lampo_conf: LampoConf = args.try_into()?;
    log::debug!(target: "lampod-cli", "init wallet ..");
    // init the logger here
    logger::init(
        &lampo_conf.log_level,
        lampo_conf
            .log_file
            .as_ref()
            .and_then(|path| Some(PathBuf::from_str(&path).unwrap())),
    )
    .expect("unable to init the logger for the first time");

    lampo_conf
        .ldk_conf
        .channel_handshake_limits
        .force_announced_channel_preference = false;
    // Prepare the backend
    let client = lampo_conf.node.clone();
    log::debug!(target: "lampod-cli", "lampo running with `{client}` backend");
    let client: Arc<dyn Backend> = match client.as_str() {
        "core" => Arc::new(BitcoinCore::new(
            &lampo_conf
                .core_url
                .clone()
                .ok_or(error::anyhow!("Miss the bitcoin url"))?,
            &lampo_conf
                .core_user
                .clone()
                .ok_or(error::anyhow!("Miss the bitcoin user for auth"))?,
            &lampo_conf
                .core_pass
                .clone()
                .ok_or(error::anyhow!("Miss the bitcoin password for auth"))?,
            Arc::new(false),
            Some(60),
        )?),
        _ => error::bail!("client {:?} not supported", client),
    };

    let wallet = if let Some(ref _private_key) = lampo_conf.private_key {
        unimplemented!()
    } else if mnemonic.is_none() {
        let (wallet, mnemonic) = match client.kind() {
            lampo_common::backend::BackendKind::Core => {
                CoreWalletManager::new(Arc::new(lampo_conf.clone()))?
            }
            lampo_common::backend::BackendKind::Nakamoto => {
                error::bail!("wallet is not implemented for nakamoto")
            }
        };

        radicle_term::success!("Wallet Generated, please store this works in a safe way");
        radicle_term::println(
            radicle_term::format::badge_primary("waller-keys"),
            format!("{}", radicle_term::format::highlight(mnemonic)),
        );
        wallet
    } else {
        match client.kind() {
            lampo_common::backend::BackendKind::Core => {
                // SAFETY: It is safe to unwrap the mnemonic because we check it
                // before.
                CoreWalletManager::restore(Arc::new(lampo_conf.clone()), &mnemonic.unwrap())?
            }
            lampo_common::backend::BackendKind::Nakamoto => {
                error::bail!("wallet is not implemented for nakamoto")
            }
        }
    };
    log::debug!(target: "lampod-cli", "wallet created with success");
    let mut lampod = LampoDeamon::new(lampo_conf.clone(), Arc::new(wallet));

    // Init the lampod
    lampod.init(client)?;

    let rpc_handler = Arc::new(CommandHandler::new(&lampo_conf)?);
    lampod.add_external_handler(rpc_handler.clone())?;

    log::debug!(target: "lampod-cli", "Lampo directory `{}`", lampo_conf.path());
    let mut _pid = filelock_rs::pid::Pid::new(lampo_conf.path(), "lampod".to_owned())
        .map_err(|err| {
            log::error!("{err}");
            error::anyhow!("impossible take a lock on the `lampod.pid` file, maybe there is another instance running?")
        })?;

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

    let workder = lampod.listen().unwrap();
    log::info!(target: "lampod-cli", "------------ Starting Server ------------");
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
    server.add_rpc("invoice", json_invoice).unwrap();
    server.add_rpc("offer", json_offer).unwrap();
    server
        .add_rpc("decode_invoice", json_decode_invoice)
        .unwrap();
    server.add_rpc("pay", json_pay).unwrap();
    server.add_rpc("keysend", json_keysend).unwrap();
    server.add_rpc("fees", json_estimate_fees).unwrap();
    server.add_rpc("close", json_close_channel).unwrap();
    let handler = server.handler();
    Ok((server.spawn(), handler))
}
