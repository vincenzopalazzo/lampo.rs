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
#[cfg(feature = "lnd")]
use lampo_lnd::{spawn as spawn_lnd_rest, LndRestConfig};
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
            // `new-wallet` must not silently reuse an existing seed. That
            // would print "wallet already exists" for a command documented
            // as creating a wallet, and a second run could look like success
            // while the node kept the old keys.
            let wallet_path = format!("{}/wallet.dat", lampo_conf.path());
            if Path::new(&wallet_path).exists() {
                error::bail!(
                    "Wallet already exists at `{wallet_path}`. \
                     Refusing to overwrite it. Remove the file only if you \
                     have backed the mnemonic up and really want a new wallet."
                );
            }
            let (_, is_new, _mnemonic) =
                BDKWalletManager::make_or_restore(lampo_conf.clone()).await?;
            if !is_new {
                error::bail!(
                    "new-wallet did not create a wallet even though `{wallet_path}` was missing"
                );
            }
            // SECURITY: do not print the mnemonic. Scrollback, tmux and CI
            // logs keep it forever. The file is mode 0600.
            println!(
                "Your new wallet mnemonic has been written to `{wallet_path}` (permissions 0600).\n\
                 PLEASE BACK IT UP SECURELY and keep it private: anyone who reads these words \
                 controls your funds."
            );
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

    let words_path = format!("{}/wallet.dat", lampo_conf.path());
    let wallet = if restore_wallet && !Path::new(&words_path).exists() {
        // Interactive restore is a CLI concern: prompt, then persist with
        // the same owner-only write the trait uses for a fresh wallet.
        let mnemonic: String = term::input(
            "BIP 39 Mnemonic",
            None,
            Some("To restore the wallet, lampo needs the BIP39 mnemonic with words separated by spaces."),
        )?;
        // FIXME: make some sanity check about the mnemonic string
        let wallet = BDKWalletManager::restore(lampo_conf.clone(), &mnemonic).await?;
        std::fs::create_dir_all(lampo_conf.path())?;
        lampo_common::wallet::write_mnemonic_file(&words_path, &mnemonic)?;
        wallet
    } else {
        // Create, or restore from the persisted mnemonic. `--restore-wallet`
        // with an existing wallet.dat is the same restore.
        let (wallet, is_new, _) = BDKWalletManager::make_or_restore(lampo_conf.clone()).await?;
        if is_new {
            log::info!(
                target: "lampod-cli",
                "New wallet created. Back up the mnemonic in `{words_path}` (mode 0600)."
            );
        } else {
            log::info!(target: "lampod-cli", "Loading from existing wallet");
        }
        wallet
    };

    // Take the pid lock before starting background wallet sync / LDK init.
    // Holding it late let a dying process keep the flock while a restart
    // burned through wallet restore and then failed with EAGAIN — and the
    // failed restart could linger because JobScheduler threads outlive main.
    log::debug!(target: "lampod-cli", "Lampo directory `{}`", lampo_conf.path());
    let mut _pid = filelock_rs::pid::Pid::new(lampo_conf.path(), "lampod".to_owned())
        .map_err(|err| {
            log::error!("{err}");
            error::anyhow!("impossible take a lock on the `lampod.pid` file, maybe there is another instance running?")
        })?;

    let wallet = Arc::new(wallet);

    log::debug!(target: "lampod-cli", "wallet created with success");
    let mut lampod = LampoDaemon::new(lampo_conf.clone(), wallet.clone());

    // Do wallet syncing in the background! (`LampoDaemon::new` already shared
    // the chain-sync coordinator with the wallet.)
    wallet.listen().await?;

    // Init the lampod
    lampod.init(client).await?;

    let lampod = Arc::new(lampod);

    if lampo_conf.lnd.unwrap_or(false) {
        #[cfg(feature = "lnd")]
        run_lnd_rest_api(lampod.clone()).await?;
        #[cfg(not(feature = "lnd"))]
        error::bail!(
            "LND compatibility was requested but lampod-cli was built without it; \
             rebuild with `--features lnd`"
        );
    } else {
        run_httpd(lampod.clone()).await?;
        let handler = Arc::new(HttpdHandler::new(format!(
            "{}:{}",
            lampo_conf.api_host, lampo_conf.api_port
        ))?);
        lampod.add_external_handler(handler).await?;
    }

    // Signal the daemon to shut down gracefully on Ctrl+C / SIGTERM.
    // This causes the LDK event processor to persist all state
    // (channel manager, scorer, network graph) before exiting.
    let shutdown_lampod = lampod.clone();
    ctrlc::set_handler(move || {
        log::info!(target: "lampod-cli", "Shutdown signal received, shutting down gracefully...");
        shutdown_lampod.shutdown();
    })?;

    log::info!(target: "lampod-cli", "------------ Starting Server ------------");
    lampod.listen().await??;
    log::info!(target: "lampod-cli", "Shutdown complete.");
    // BDK's JobScheduler keeps non-daemon threads alive after listen()
    // returns, so returning normally would leave this process (and the
    // pid flock) hung forever and block any subsequent start. Exit so the
    // OS releases the flock.
    std::process::exit(0);
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

#[cfg(feature = "lnd")]
pub async fn run_lnd_rest_api(lampod: Arc<LampoDaemon>) -> error::Result<()> {
    let conf = lampod.conf();
    let host = conf
        .api_host
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_string();
    let port = lnd_api_port(conf.api_port)?;
    let data = conf.path();
    let tls_dir = format!("{data}/lnd-rest");
    let macaroon_dir = format!("{data}/lnd-rest/macaroons");

    let lnd_conf = LndRestConfig {
        listen_host: host.clone(),
        listen_port: port,
        tls_extra_sans: conf.lnd_tls_sans.clone(),
        tls_dir: tls_dir.into(),
        macaroon_dir: macaroon_dir.clone().into(),
    };

    spawn_lnd_rest(lampod, lnd_conf)?;
    log::info!(
        target: "lampod-cli",
        "LND API ready on https://{}:{} (macaroons under {})",
        host,
        port,
        macaroon_dir
    );
    Ok(())
}

#[cfg(feature = "lnd")]
fn lnd_api_port(port: u64) -> error::Result<u16> {
    let port =
        u16::try_from(port).map_err(|_| error::anyhow!("api-port must be between 1 and 65535"))?;
    if port == 0 {
        error::bail!("api-port must be between 1 and 65535");
    }
    Ok(port)
}

#[cfg(all(test, feature = "lnd"))]
mod tests {
    use super::lnd_api_port;

    #[test]
    fn lnd_api_port_rejects_zero_and_overflow() {
        assert!(lnd_api_port(0).is_err());
        assert!(lnd_api_port(u16::MAX as u64 + 1).is_err());
        assert_eq!(lnd_api_port(8080).unwrap(), 8080);
    }
}
