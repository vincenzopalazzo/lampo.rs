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
#[cfg(feature = "lnd")]
use lampo_lnd::{spawn as spawn_lnd_rest, LndRestConfig};
use lampo_plugin::tls::CertStore;
use lampo_plugin::transport::grpc::GrpcConfig;
use lampo_plugin::PluginManager;
use lampo_plugin_common::messages::InitConfig;
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

    // Chain sync calls bitcoind during `init`, before the event handler used
    // to be installed. Start the plugin and attach a dispatcher first, or
    // `getblockchaininfo` fails with "chain handler not set".
    let node_id = {
        use lampo_common::ldk::sign::NodeSigner;
        wallet
            .ldk_keys()
            .inner()
            .get_node_id(lampo_common::ldk::sign::Recipient::Node)
            .map(|id| id.to_string())
            .unwrap_or_default()
    };
    let plugin_manager = Arc::new(start_plugins(&lampo_conf, &node_id).await?);
    client.set_handler(plugin_manager.clone());

    // Do wallet syncing in the background! (`LampoDaemon::new` already shared
    // the chain-sync coordinator with the wallet.)
    wallet.listen().await?;

    // Init the lampod
    lampod.init(client).await?;

    let lampod = Arc::new(lampod);

    // Plugin methods before httpd, so `lampo-cli foo` hits the plugin first.
    lampod.add_external_handler(plugin_manager.clone()).await?;
    lampod.set_plugin_manager(plugin_manager.clone()).await?;

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
    plugin_manager.shutdown_all().await;
    // BDK's JobScheduler keeps non-daemon threads alive after listen()
    // returns, so returning normally would leave this process (and the
    // pid flock) hung forever and block any subsequent start. Exit so the
    // OS releases the flock.
    std::process::exit(0);
}

/// Discover and start all plugins from config and CLI args.
/// `lampo-bitcoind` next to this binary, then `target/release` / `target/debug`.
fn bitcoind_init(conf: &LampoConf, base: &InitConfig) -> InitConfig {
    let mut init = base.clone();
    if let Some(url) = &conf.core_url {
        init.options
            .insert("core-url".into(), lampo_common::json::json!(url));
    }
    if let Some(user) = &conf.core_user {
        init.options
            .insert("core-user".into(), lampo_common::json::json!(user));
    }
    if let Some(pass) = &conf.core_pass {
        init.options
            .insert("core-pass".into(), lampo_common::json::json!(pass));
    }
    init
}

fn default_bitcoind_plugin() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for candidate in [
        dir.join("lampo-bitcoind"),
        dir.join("../release/lampo-bitcoind"),
        dir.join("../debug/lampo-bitcoind"),
    ] {
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

async fn start_plugins(conf: &LampoConf, node_id: &str) -> error::Result<PluginManager> {
    let manager = PluginManager::new();

    let init_config = InitConfig {
        lampo_dir: conf.path(),
        network: conf.network.to_string(),
        node_id: node_id.to_owned(),
        options: lampo_common::json::Map::new(),
    };

    // Collect plugin paths from explicit --plugin args and plugin-dir
    let mut plugin_paths: Vec<String> = conf.plugins.clone();

    // Scan plugin directory if configured
    if let Some(ref dir) = conf.plugin_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    // Check if the file is executable (Unix)
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Ok(metadata) = path.metadata() {
                            if metadata.permissions().mode() & 0o111 != 0 {
                                if let Some(p) = path.to_str() {
                                    plugin_paths.push(p.to_string());
                                }
                            }
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        if let Some(p) = path.to_str() {
                            plugin_paths.push(p.to_string());
                        }
                    }
                }
            }
        } else {
            log::warn!(target: "plugin", "plugin directory `{}` not found", dir);
        }
    }

    // Default chain backend. Same role as CLN's shipped plugins: present
    // unless the operator already passed a plugin that will register the
    // bitcoind methods.
    if plugin_paths.is_empty() {
        if let Some(default_plugin) = default_bitcoind_plugin() {
            plugin_paths.push(default_plugin);
        }
    }

    // Start each plugin. An `important` plugin (CLN's flag) aborts startup.
    for plugin_path in &plugin_paths {
        let plugin_init = if plugin_path.contains("lampo-bitcoind") {
            bitcoind_init(conf, &init_config)
        } else {
            init_config.clone()
        };
        match manager.start_plugin(plugin_path, &plugin_init).await {
            Ok(name) => {
                log::info!(target: "lampod-cli", "plugin `{}` started", name);
            }
            Err(e) => {
                log::error!(target: "lampod-cli", "failed to start plugin `{}`: {}", plugin_path, e);
                if manager.is_important_path(plugin_path).await {
                    error::bail!("important plugin `{plugin_path}` failed to start: {e}");
                }
            }
        }
    }

    // Start remote plugins via gRPC
    if !conf.remote_plugins.is_empty() {
        // Initialize TLS certificates for mTLS
        let cert_store = CertStore::new(&conf.path());
        cert_store.ensure_initialized()?;

        for endpoint in &conf.remote_plugins {
            let grpc_config = GrpcConfig {
                endpoint: endpoint.clone(),
                ca_cert_pem: Some(cert_store.ca_cert_pem()?),
                client_cert_pem: Some(cert_store.client_cert_pem()?),
                client_key_pem: Some(cert_store.client_key_pem()?),
            };
            match manager.start_remote_plugin(grpc_config, &init_config).await {
                Ok(name) => {
                    log::info!(target: "lampod-cli", "remote plugin `{}` started", name);
                }
                Err(e) => {
                    log::error!(
                        target: "lampod-cli",
                        "failed to start remote plugin `{}`: {}",
                        endpoint, e
                    );
                }
            }
        }
    }

    let total = manager.list_plugins().await.len();
    if total > 0 {
        log::info!(
            target: "lampod-cli",
            "started {} plugin(s): {:?}",
            total,
            manager.list_plugins().await
        );
    }

    Ok(manager)
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
