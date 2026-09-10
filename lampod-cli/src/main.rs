#[allow(dead_code)]
mod args;

use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
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
use lampo_httpd::handler::HttpdHandler;
use lampod::chain::WalletManager;
use lampod::LampoDaemon;

use crate::args::LampoCliArgs;

#[tokio::main]
async fn main() -> error::Result<()> {
    log::debug!("Started!");
    let args = args::parse_args()?;
    match &args.subcommand {
        Some(crate::args::LampoCliSubcommand::NewWallet) => {
            // Prepare minimal config for wallet creation (no logger needed)
            let mut lampo_conf: LampoConf = args.clone().try_into()?;
            lampo_conf
                .ldk_conf
                .channel_handshake_limits
                .force_announced_channel_preference = false;
            let lampo_conf = Arc::new(lampo_conf);
            let client = lampo_conf.node.clone();
            let client: Arc<dyn Backend> = match client.as_str() {
                "core" => Arc::new(LampoChainSync::new(lampo_conf.clone())?),
                _ => error::bail!("client {:?} not supported", client),
            };
            let words_path = format!("{}/", lampo_conf.path());
            create_new_wallet(lampo_conf, client, &words_path).await?;
            return Ok(());
        }
        _ => run(args).await,
    }
}

fn write_words_to_file<P: AsRef<Path>>(path: P, words: String) -> error::Result<()> {
    // SECURITY: `path` will hold the BIP39 mnemonic. Create it owner-only
    // (0600); the default umask (022) would otherwise leave it
    // world-readable (0644) for any local user.
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path.as_ref())?;

    // FIXME: we should give the possibility to encrypt this file.
    file.write_all(words.as_bytes())?;

    // `OpenOptions::mode` only applies when the file is created (and is
    // masked by the umask), so tighten the permissions explicitly to also
    // cover pre-existing files with looser modes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path.as_ref(), std::fs::Permissions::from_mode(0o600))?;
    }
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

async fn create_new_wallet(
    lampo_conf: Arc<LampoConf>,
    client: Arc<dyn Backend>,
    words_path: &str,
) -> error::Result<Arc<dyn WalletManager>> {
    let (wallet, mnemonic) = match client.kind() {
        lampo_common::backend::BackendKind::Core => {
            BDKWalletManager::new(lampo_conf.clone()).await?
        }
    };
    let wallet_path = format!("{}/wallet.dat", words_path);
    write_words_to_file(&wallet_path, mnemonic.clone())?;
    // SECURITY: do not print the mnemonic to the terminal -- it would leak
    // into scrollback buffers, tmux/screen logs, CI logs and `ps`-visible
    // transcripts. Point the user at the (0600) wallet file instead.
    println!(
        "Your new wallet mnemonic has been written to `{wallet_path}` (permissions 0600).\n\
         PLEASE BACK IT UP SECURELY and keep it private: anyone who reads these words \
         controls your funds."
    );
    Ok(Arc::new(wallet))
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

    let words_path = format!("{}/", lampo_conf.path());
    let wallet = if restore_wallet {
        if Path::new(&format!("{}/wallet.dat", words_path)).exists() {
            // Load the mnemonic from the file
            let mnemonic = load_words_from_file(format!("{}/wallet.dat", words_path))?;
            let wallet = match client.kind() {
                lampo_common::backend::BackendKind::Core => {
                    BDKWalletManager::restore(lampo_conf.clone(), &mnemonic).await?
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
                    BDKWalletManager::restore(lampo_conf.clone(), &mnemonic).await?
                }
            };
            write_words_to_file(format!("{}/wallet.dat", words_path), mnemonic)?;
            wallet
        }
    } else {
        if Path::new(&format!("{}/wallet.dat", words_path)).exists() {
            // Load the mnemonic from the file
            log::warn!("Loading from existing wallet");
            let mnemonic = load_words_from_file(format!("{}/wallet.dat", words_path))?;
            let wallet = match client.kind() {
                lampo_common::backend::BackendKind::Core => {
                    BDKWalletManager::restore(lampo_conf.clone(), &mnemonic).await?
                }
            };
            wallet
        } else {
            // Use the new function for wallet creation
            create_new_wallet(lampo_conf.clone(), client.clone(), &words_path).await?;
            return Ok(());
        }
    };

    let wallet = Arc::new(wallet);

    log::debug!(target: "lampod-cli", "wallet created with success");
    let mut lampod = LampoDaemon::new(lampo_conf.clone(), wallet.clone());

    // Do wallet syncing in the background! (`LampoDaemon::new` already shared
    // the chain-sync coordinator with the wallet.)
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

    // Signal the daemon to shut down gracefully on Ctrl+C.
    // This causes the LDK event processor to persist all state
    // (channel manager, scorer, network graph) before exiting.
    let shutdown_lampod = lampod.clone();
    ctrlc::set_handler(move || {
        log::info!("Shutdown signal received, shutting down gracefully...");
        shutdown_lampod.shutdown();
    })?;

    log::info!(target: "lampod-cli", "------------ Starting Server ------------");
    lampod.listen().await??;
    log::info!(target: "lampod-cli", "Shutdown complete.");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// REPRO (bug #2): `wallet.dat` holds the BIP39 mnemonic but is created
    /// with default `OpenOptions` (no explicit mode), so with the typical
    /// umask 022 it ends up world-readable (0644). Any local user can read
    /// the node's wallet seed.
    #[cfg(unix)]
    #[test]
    fn wallet_dat_is_written_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("lampod-cli-repro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wallet_path = dir.join("wallet.dat");

        write_words_to_file(&wallet_path, "abandon abandon abandon".to_string()).unwrap();

        let mode = std::fs::metadata(&wallet_path)
            .unwrap()
            .permissions()
            .mode();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            mode & 0o777,
            0o600,
            "wallet.dat contains the mnemonic and must be 0600, got {:o}",
            mode & 0o777
        );
    }

    /// Regression: pre-existing wallet.dat with loose permissions must be
    /// tightened to 0600 as well.
    #[cfg(unix)]
    #[test]
    fn wallet_dat_permissions_are_tightened_on_existing_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("lampod-cli-repro2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wallet_path = dir.join("wallet.dat");
        std::fs::write(&wallet_path, "old words").unwrap();
        std::fs::set_permissions(&wallet_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_words_to_file(&wallet_path, "abandon abandon abandon".to_string()).unwrap();

        let mode = std::fs::metadata(&wallet_path)
            .unwrap()
            .permissions()
            .mode();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            mode & 0o777,
            0o600,
            "existing wallet.dat must be tightened to 0600, got {:o}",
            mode & 0o777
        );
    }
}
