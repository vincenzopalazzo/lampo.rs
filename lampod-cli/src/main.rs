#[allow(dead_code)]
mod args;

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use lampo_chain::LampoChainSync;
use lampo_httpd::handler::HttpdHandler;
use radicle_term as term;

use lampo_common::backend::Backend;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::logger;
use lampo_core_wallet::CoreWalletManager;
use lampod::chain::WalletManager;
use lampod::LampoDaemon;

use crate::args::LampoCliArgs;

#[tokio::main]
async fn main() -> error::Result<()> {
    log::debug!("Started!");
    let args = args::parse_args()?;
    run(args).await?;
    Ok(())
}

/// Return the root directory.
async fn run(args: LampoCliArgs) -> error::Result<()> {
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

    let lampo_conf = Arc::new(lampo_conf);

    // Prepare the backend
    let client = lampo_conf.node.clone();
    log::debug!(target: "lampod-cli", "lampo running with `{client}` backend");
    let client: Arc<dyn Backend> = match client.as_str() {
        "core" => Arc::new(LampoChainSync::new(lampo_conf.clone())?),
        _ => error::bail!("client {:?} not supported", client),
    };

    let wallet = if let Some(ref priv_key) = lampo_conf.private_key {
        #[cfg(debug_assertions)]
        {
            let Ok(key) = lampo_common::secp256k1::SecretKey::from_str(priv_key) else {
                error::bail!("invalid private key `{priv_key}`");
            };
            let key = lampo_common::bitcoin::PrivateKey::new(key, lampo_conf.network);
            let wallet = CoreWalletManager::try_from((
                key,
                lampo_conf.channels_keys.clone(),
                lampo_conf.clone(),
            ));
            let Ok(wallet) = wallet else {
                error::bail!("error while create the wallet: `{}`", wallet.err().unwrap());
            };
            wallet
        }
        #[cfg(not(debug_assertions))]
        unimplemented!()
    } else if mnemonic.is_none() {
        let (wallet, mnemonic) = match client.kind() {
            lampo_common::backend::BackendKind::Core => CoreWalletManager::new(lampo_conf.clone())?,
            lampo_common::backend::BackendKind::Nakamoto => {
                error::bail!("wallet is not implemented for nakamoto")
            }
        };

        radicle_term::success!("Wallet Generated, please store these words in a safe way");
        radicle_term::println(
            radicle_term::format::badge_primary("wallet-keys"),
            format!("{}", radicle_term::format::highlight(mnemonic)),
        );
        wallet
    } else {
        match client.kind() {
            lampo_common::backend::BackendKind::Core => {
                // SAFETY: It is safe to unwrap the mnemonic because we check it
                // before.
                CoreWalletManager::restore(lampo_conf.clone(), &mnemonic.unwrap())?
            }
            lampo_common::backend::BackendKind::Nakamoto => {
                error::bail!("wallet is not implemented for nakamoto")
            }
        }
    };
    log::debug!(target: "lampod-cli", "wallet created with success");
    let mut lampod = LampoDaemon::new(lampo_conf.clone(), Arc::new(wallet));

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
    lampod.add_external_handler(handler)?;

    ctrlc::set_handler(move || {
        use std::time::Duration;
        log::info!("Shutdown...");
        std::thread::sleep(Duration::from_secs(5));
        std::process::exit(0);
    })?;

    lampod.listen();
    log::info!(target: "lampod-cli", "------------ Starting Server ------------");
    // FIXME: this should not block on, but the listen should be blocking the execution
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
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
