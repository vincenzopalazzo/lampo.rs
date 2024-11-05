#[allow(dead_code)]
mod args;

use std::env;
use std::io;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::thread::JoinHandle;

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

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
use lampod::LampoDaemon;

use crate::args::LampoCliArgs;

fn main() -> error::Result<()> {
    log::debug!("Started!");
    let args = args::parse_args()?;
    run(args)?;
    Ok(())
}

fn write_words_to_file<P: AsRef<Path>>(path: P, words: String) -> error::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;

    // FIXME: we should give the possibility to encrypt this file.
    file.write_all(words.as_bytes())?;
    Ok(())
}

fn load_words_from_file<P: AsRef<Path>>(path: P) -> error::Result<String> {
    let mut file = File::open(path.as_ref())?;
    let mut content = String::new();

    file.read_to_string(&mut content)?;

    if content.is_empty() {
        let path = path.as_ref().to_string_lossy().to_string();
        error::bail!("The content of the wallet located at `{path}`. You lost the secret? Please report a bug this should never happens")
    } else {
        Ok(content)
    }
}

/// Return the root directory.
fn run(args: LampoCliArgs) -> error::Result<()> {
    let restore_wallet = args.restore_wallet;

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

    if let Some(ref _private_key) = lampo_conf.private_key {
        error::bail!("Option to force a private key not available at the moment")
    }

    let words_path = format!("{}/", lampo_conf.path());
    // There are several case in this if-else, that are:
    // 1. lampo is running on a fresh os:
    //   1.1: the user has a wallet, so need to specify `--restore-wallet`
    //   1.2: the user does not have a wallet so lampo should generate a new seed for it and store it in a file.
    // 2. lampo is running on os where there is a wallet, lampo will load the seeds from wallet:
    //   2.1: The user keep specify --restore-wallet, so lampo should return an error an tell the user that there is already a wallet
    //   2.2: The user do not specify the --restore-wallet, so lampo load from disk the file, if there is no file it return an error
    // FIXME: there is a problem of code duplication here, we should move this code in utils functions.
    let wallet = if restore_wallet {
        if Path::new(&format!("{}/wallet.dat", words_path)).exists() {
            // Load the mnemonic from the file
            let mnemonic = load_words_from_file(format!("{}/wallet.dat", words_path))?;
            let wallet = match client.kind() {
                lampo_common::backend::BackendKind::Core => {
                    CoreWalletManager::restore(Arc::new(lampo_conf.clone()), &mnemonic)?
                }
                lampo_common::backend::BackendKind::Nakamoto => {
                    error::bail!("wallet is not implemented for nakamoto")
                }
            };
            wallet
        } else {
            // If file doesn't exist, ask for user input
            let mnemonic: String = term::input(
                "BIP 39 Mnemonic",
                None,
                Some("To restore the wallet, lampo needs the BIP39 mnemonic with words separated by spaces."),
            )?;
            // FIXME: make some sanity check about the mnemonic string
            let wallet = match client.kind() {
                lampo_common::backend::BackendKind::Core => {
                    // SAFETY: It is safe to unwrap the mnemonic because we check it
                    // before.
                    CoreWalletManager::restore(Arc::new(lampo_conf.clone()), &mnemonic)?
                }
                lampo_common::backend::BackendKind::Nakamoto => {
                    error::bail!("wallet is not implemented for nakamoto")
                }
            };

            write_words_to_file(format!("{}/wallet.dat", words_path), mnemonic)?;
            wallet
        }
    } else {
        // If there is a file, we load the wallet with a warning
        if Path::new(&format!("{}/wallet.dat", words_path)).exists() {
            // Load the mnemonic from the file
            log::warn!("Loading from existing wallet");
            let mnemonic = load_words_from_file(format!("{}/wallet.dat", words_path))?;
            let wallet = match client.kind() {
                lampo_common::backend::BackendKind::Core => {
                    CoreWalletManager::restore(Arc::new(lampo_conf.clone()), &mnemonic)?
                }
                lampo_common::backend::BackendKind::Nakamoto => {
                    error::bail!("wallet is not implemented for nakamoto")
                }
            };
            wallet
        } else {
            let (wallet, mnemonic) = match client.kind() {
                lampo_common::backend::BackendKind::Core => {
                    CoreWalletManager::new(Arc::new(lampo_conf.clone()))?
                }
                lampo_common::backend::BackendKind::Nakamoto => {
                    error::bail!("wallet is not implemented for nakamoto")
                }
            };

            write_words_to_file(format!("{}/wallet.dat", words_path), mnemonic)?;
            wallet
        }
    };

    log::debug!(target: "lampod-cli", "wallet created with success");
    let mut lampod = LampoDaemon::new(lampo_conf.clone(), Arc::new(wallet));

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
    lampod: Arc<LampoDaemon>,
) -> error::Result<(JoinHandle<io::Result<()>>, Arc<Handler<LampoDaemon>>)> {
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
    server.add_rpc("decode", json_decode_invoice).unwrap();
    server.add_rpc("pay", json_pay).unwrap();
    server.add_rpc("keysend", json_keysend).unwrap();
    server.add_rpc("fees", json_estimate_fees).unwrap();
    server.add_rpc("close", json_close_channel).unwrap();
    let handler = server.handler();
    Ok((server.spawn(), handler))
}
