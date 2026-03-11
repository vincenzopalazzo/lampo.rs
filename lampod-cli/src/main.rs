#[allow(dead_code)]
mod args;

use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use radicle_term as term;

use lampo_bdk_wallet::BDKWalletManager;
use lampo_chain::LampoChainSync;
use lampo_common::backend::Backend;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::logger;
use lampo_common::wallet::WalletManager;
use lampo_httpd::handler::HttpdHandler;
use lampod::LampoDaemon;

use crate::args::LampoCliArgs;

#[tokio::main]
async fn main() -> error::Result<()> {
    log::debug!("Started!");
    let args = args::parse_args()?;
    match &args.subcommand {
        Some(crate::args::LampoCliSubcommand::NewWallet) => {
            let mut lampo_conf: LampoConf = args.clone().try_into()?;
            lampo_conf
                .ldk_conf
                .channel_handshake_limits
                .force_announced_channel_preference = false;
            let lampo_conf = Arc::new(lampo_conf);
            let (_, is_new) = BDKWalletManager::make_or_restore(lampo_conf.clone()).await?;
            if is_new {
                let mnemonic =
                    std::fs::read_to_string(format!("{}/wallet.dat", lampo_conf.path()))?;
                println!("Your new wallet mnemonic is:\n{mnemonic}\nPLEASE BACK IT UP SECURELY!");
            } else {
                println!("Wallet already exists, loaded from existing mnemonic.");
            }
            return Ok(());
        }
        _ => run(args).await,
    }
}

/// Return the root directory.
async fn run(args: LampoCliArgs) -> error::Result<()> {
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

    let lampo_conf = Arc::new(lampo_conf);

    // Prepare the backend
    let client = lampo_conf.node.clone();
    log::debug!(target: "lampod-cli", "lampo running with `{client}` backend");
    let client: Arc<dyn Backend> = match client.as_str() {
        "core" => Arc::new(LampoChainSync::new(lampo_conf.clone())?),
        _ => error::bail!("client {:?} not supported", client),
    };

    let wallet = if restore_wallet
        && !Path::new(&format!("{}/wallet.dat", lampo_conf.path())).exists()
    {
        // Interactive restore: prompt the user for their mnemonic
        let mnemonic: String = term::input(
            "BIP 39 Mnemonic",
            None,
            Some("To restore the wallet, lampo needs the BIP39 mnemonic with words separated by spaces."),
        )?;
        // FIXME: make some sanity check about the mnemonic string
        let wallet = BDKWalletManager::restore(lampo_conf.clone(), &mnemonic).await?;
        std::fs::create_dir_all(lampo_conf.path())?;
        std::fs::write(format!("{}/wallet.dat", lampo_conf.path()), &mnemonic)?;
        wallet
    } else {
        // Common path: create or restore from persisted wallet.dat
        let (wallet, is_new) = BDKWalletManager::make_or_restore(lampo_conf.clone()).await?;
        if is_new {
            log::info!(target: "lampod-cli", "New wallet created. Back up your mnemonic!");
        } else {
            log::info!(target: "lampod-cli", "Loading from existing wallet");
        }
        wallet
    };

    let wallet = Arc::new(wallet);

    log::debug!(target: "lampod-cli", "wallet created with success");
    let mut lampod = LampoDaemon::new(lampo_conf.clone(), wallet.clone());

    // Do wallet syncing in the background!
    wallet.listen().await?;

    // Init the lampod
    lampod.init(client).await?;

    log::debug!(target: "lampod-cli", "Lampo directory `{}`", lampo_conf.path());
    let mut _pid = filelock_rs::pid::Pid::new(lampo_conf.path(), "lampod".to_owned())
        .map_err(|err| {
            log::error!("{err}");
            error::anyhow!("impossible take a lock on the `lampod.pid` file, maybe there is another instance running?")
        })?;

    let lampod = Arc::new(lampod);

    run_httpd(lampod.clone()).await?;

    let handler = Arc::new(HttpdHandler::new(format!(
        "{}:{}",
        lampo_conf.api_host, lampo_conf.api_port
    ))?);
    lampod.add_external_handler(handler).await?;

    // Handle the shutdown signal and pass down to the lampod.listen()
    ctrlc::set_handler(move || {
        use std::time::Duration;
        log::info!("Shutdown...");
        std::thread::sleep(Duration::from_secs(5));
        std::process::exit(0);
    })?;

    log::info!(target: "lampod-cli", "------------ Starting Server ------------");
    lampod.listen().await??;
    Ok(())
}

pub async fn run_httpd(lampod: Arc<LampoDaemon>) -> error::Result<()> {
    let url = format!("{}:{}", lampod.conf().api_host, lampod.conf().api_port);
    let mut http_hosting = url.clone();
    if let Some(clean_url) = url.strip_prefix("http://") {
        http_hosting = clean_url.to_string();
    } else if let Some(clean_url) = url.strip_prefix("https://") {
        http_hosting = clean_url.to_string();
    }
    log::info!("preparing httpd api on addr `{url}`");
    tokio::spawn(lampo_httpd::run(lampod, http_hosting, url));
    Ok(())
}
