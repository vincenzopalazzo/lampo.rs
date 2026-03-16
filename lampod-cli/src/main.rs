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
use lampo_plugin::PluginManager;
use lampo_plugin::tls::CertStore;
use lampo_plugin::transport::grpc::GrpcConfig;
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
    write_words_to_file(format!("{}/wallet.dat", words_path), mnemonic.clone())?;
    println!("Your new wallet mnemonic is:\n{mnemonic}\nPLEASE BACK IT UP SECURELY!");
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

    // Start plugins before httpd so plugin methods get priority
    let plugin_manager = Arc::new(start_plugins(&lampo_conf).await?);
    lampod
        .add_external_handler(plugin_manager.clone())
        .await?;
    // Wire plugin manager into handler for hooks and notifications
    lampod.set_plugin_manager(plugin_manager).await?;

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

/// Discover and start all plugins from config and CLI args.
async fn start_plugins(conf: &LampoConf) -> error::Result<PluginManager> {
    let manager = PluginManager::new();

    let init_config = InitConfig {
        lampo_dir: conf.path(),
        network: conf.network.to_string(),
        node_id: String::new(),
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

    // Start each plugin
    for plugin_path in &plugin_paths {
        match manager.start_plugin(plugin_path, &init_config).await {
            Ok(name) => {
                log::info!(target: "lampod-cli", "plugin `{}` started", name);
            }
            Err(e) => {
                log::error!(target: "lampod-cli", "failed to start plugin `{}`: {}", plugin_path, e);
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
                ca_cert_pem: cert_store.ca_cert_pem().ok(),
                client_cert_pem: cert_store.client_cert_pem().ok(),
                client_key_pem: cert_store.client_key_pem().ok(),
            };
            match manager
                .start_remote_plugin(grpc_config, &init_config)
                .await
            {
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
