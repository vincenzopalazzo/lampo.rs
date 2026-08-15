//! lampo-spark-swapd: a swap daemon between a lampo lightning node
//! (BOLT12 offers) and Spark, both embedded in this process.
//!
//! The startup mirrors `lampod-cli`: the daemon *is* the node, plus a
//! Spark wallet and the swap engine wired to the node's in-process
//! event stream. Run this **instead of** `lampod-cli`, never both:
//! they share the `lampod.pid` lock for exactly that reason.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use clap::Parser;

use lampo_bdk_wallet::BDKWalletManager;
use lampo_chain::LampoChainSync;
use lampo_common::backend::Backend;
use lampo_common::conf::{LampoConf, Network};
use lampo_common::error;
use lampo_common::logger;
use lampo_httpd::handler::HttpdHandler;
use lampod::chain::WalletManager;
use lampod::LampoDaemon;
use spark::signer::{DefaultSigner, SparkSignerAdapter};
use spark_wallet::{OperatorPoolConfig, SparkWalletConfig, WalletBuilder};

use lampo_spark_swapd::api;
use lampo_spark_swapd::engine::Engine;
use lampo_spark_swapd::lampo_leg::LampoLeg;
use lampo_spark_swapd::settings::Settings;
use lampo_spark_swapd::spark_leg::SparkLeg;
use lampo_spark_swapd::store::SwapStore;

#[derive(Parser)]
#[command(name = "lampo-spark-swapd", about = "lampo <-> spark swap daemon")]
struct Args {
    /// Root data directory of the lampo node, the one holding
    /// `lampo.conf`. Swap settings are read from that same file.
    #[arg(long)]
    data_dir: Option<String>,
    /// Lampo network: mainnet, signet (incl. mutinynet), testnet, or
    /// regtest. Only mainnet has SDK-hosted spark operators; the others
    /// need self-hosted `spark-operator` lines. Defaults to regtest so a
    /// daemon started with no flags comes up rather than failing on the
    /// spark leg.
    #[arg(long, default_value = "regtest")]
    network: String,
}

#[tokio::main]
async fn main() -> error::Result<()> {
    let args = Args::parse();

    // --- lampo node, exactly as lampod-cli builds it ---
    let mut lampo_conf = LampoConf::new(
        args.data_dir.clone(),
        Some(parse_ln_network(&args.network)?),
        None,
    )?;
    logger::init(&lampo_conf.log_level, None).map_err(|err| error::anyhow!("{err}"))?;
    lampo_conf
        .ldk_conf
        .channel_handshake_limits
        .force_announced_channel_preference = false;
    let lampo_conf = Arc::new(lampo_conf);

    let settings = Settings::from_lampo_conf(&lampo_conf)?;
    std::fs::create_dir_all(Settings::swapd_dir(&lampo_conf))?;

    let client: Arc<dyn Backend> = match lampo_conf.node.as_str() {
        "core" => Arc::new(LampoChainSync::new(lampo_conf.clone())?),
        node => error::bail!("backend `{node}` not supported"),
    };

    let words_file = format!("{}/wallet.dat", lampo_conf.path());
    let wallet = if Path::new(&words_file).exists() {
        let mnemonic = read_to_string(&words_file)?;
        BDKWalletManager::restore(lampo_conf.clone(), mnemonic.trim()).await?
    } else {
        let (wallet, mnemonic) = BDKWalletManager::new(lampo_conf.clone()).await?;
        std::fs::write(&words_file, &mnemonic)?;
        log::warn!(target: "swapd", "created a new lampo wallet, mnemonic written to `{words_file}` - BACK IT UP");
        wallet
    };
    let wallet: Arc<dyn WalletManager> = Arc::new(wallet);

    let mut lampod = LampoDaemon::new(lampo_conf.clone(), wallet.clone());
    wallet.listen().await?;
    lampod.init(client).await?;

    // The daemon *is* the node, so it takes the same `lampod.pid` lock
    // `lampod-cli` takes. Two processes running LDK against one channel
    // state is a fund-loss bug, and swapd and lampod-cli are exactly
    // that if both are started on the same data directory.
    let _pid = filelock_rs::pid::Pid::new(lampo_conf.path(), "lampod".to_owned()).map_err(|err| {
        log::error!(target: "swapd", "{err}");
        error::anyhow!(
            "cannot lock `lampod.pid` in `{}`: another lampod or swapd is already running on this data directory",
            lampo_conf.path()
        )
    })?;

    let lampod = Arc::new(lampod);

    // lampo-httpd must run: `LampoHandler::call` routes through the
    // registered external handler, which posts to the node's own API.
    run_httpd(lampod.clone()).await?;
    let handler = Arc::new(HttpdHandler::new(format!(
        "{}:{}",
        lampo_conf.api_host, lampo_conf.api_port
    ))?);
    lampod.add_external_handler(handler).await?;

    // --- spark wallet ---
    let seed = load_or_create_seed(&settings)?;
    let spark_network = parse_spark_network(&settings.spark_network)?;
    let signer = Arc::new(SparkSignerAdapter::new(Arc::new(
        DefaultSigner::new(&seed, spark_network)
            .map_err(|err| error::anyhow!("spark signer: {err}"))?,
    )));
    let mut spark_config = SparkWalletConfig::default_config(spark_network);
    if !settings.spark_operators.is_empty() {
        // A local regtest stack: the SDK defaults point at Lightspark's
        // hosted operators, which a self hosted network must override.
        let operators = settings
            .spark_operators
            .iter()
            .map(|operator| {
                SparkWalletConfig::create_operator_config(
                    operator.id,
                    &operator.identifier,
                    &operator.address,
                    operator.ca_cert.as_deref(),
                    &operator.identity_public_key,
                )
                .map_err(|err| error::anyhow!("spark operator `{}`: {err}", operator.id))
            })
            .collect::<error::Result<Vec<_>>>()?;
        let coordinator = operators
            .first()
            .map(|operator| operator.id)
            .ok_or(error::anyhow!("no spark operators configured"))?;
        log::info!(target: "swapd", "using {} local spark operators", operators.len());
        spark_config.operator_pool = OperatorPoolConfig::new(coordinator, operators)
            .map_err(|err| error::anyhow!("spark operator pool: {err}"))?;
        spark_config
            .validate()
            .map_err(|err| error::anyhow!("spark config: {err}"))?;
    }
    let spark_wallet = Arc::new(
        WalletBuilder::new(spark_config, signer)
            .build()
            .await
            .map_err(|err| error::anyhow!("spark wallet: {err}"))?,
    );
    log::info!(target: "swapd", "spark wallet up, address `{}`",
        SparkLeg::new(spark_wallet.clone()).spark_address().await?);

    // --- swap engine ---
    let store = SwapStore::new(Settings::swapd_dir(&lampo_conf).join("swaps"))?;
    let engine = Arc::new(Engine::new(
        LampoLeg::new(lampod.handler()),
        SparkLeg::new(spark_wallet),
        store,
        settings.clone(),
    ));
    tokio::spawn(engine.clone().run());
    tokio::spawn(api::run(engine, settings.api_addr.clone()));

    let shutdown = lampod.clone();
    ctrlc::set_handler(move || {
        log::info!(target: "swapd", "shutting down...");
        shutdown.shutdown();
    })?;

    log::info!(target: "swapd", "------------ swapd running ------------");
    lampod.listen().await??;
    Ok(())
}

fn read_to_string(path: &str) -> error::Result<String> {
    let mut content = String::new();
    std::fs::File::open(path)?.read_to_string(&mut content)?;
    Ok(content)
}

/// Read the Spark seed, creating it with fresh kernel entropy when
/// missing. 32 bytes, hex on disk, same custody story as lampo's
/// `wallet.dat`.
fn load_or_create_seed(settings: &Settings) -> error::Result<Vec<u8>> {
    let path = &settings.spark_seed_file;
    if path.exists() {
        // Refuse to run on a seed anyone else on the box can read. This
        // catches seeds created before the 0600 default and seeds copied
        // in by hand -- silently using one would be custody theatre.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path)?.permissions().mode() & 0o077;
            if mode != 0 {
                error::bail!(
                    "the spark seed `{path:?}` is group/world accessible; run `chmod 600` on it"
                );
            }
        }
        let content = std::fs::read_to_string(path)?;
        let content = content.trim();
        if content.len() != 64 {
            error::bail!(
                "the spark seed must be 32 bytes of hex, found {} chars",
                content.len()
            );
        }
        let bytes = (0..content.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&content[i..i + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|err| error::anyhow!("corrupted spark seed file: {err}"))?;
        return Ok(bytes);
    }
    let mut seed = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut seed)?;
    let hex: String = seed.iter().map(|b| format!("{b:02x}")).collect();
    // Create it 0600 *before* the secret goes in: whoever holds this
    // file holds every sat the swap wallet controls, and a default-mode
    // write leaves it readable by every user on the box. Creating with
    // the mode set avoids the window a write-then-chmod would open.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(hex.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, &hex)?;
    log::warn!(target: "swapd", "created a new spark seed at `{path:?}` - BACK IT UP");
    Ok(seed.to_vec())
}

fn parse_ln_network(network: &str) -> error::Result<Network> {
    use std::str::FromStr;
    Network::from_str(network).map_err(|err| error::anyhow!("invalid lampo network: {err}"))
}

fn parse_spark_network(network: &str) -> error::Result<spark::Network> {
    match network {
        "mainnet" | "bitcoin" => Ok(spark::Network::Mainnet),
        // signet covers mutinynet, which is a signet with fast blocks:
        // the network params are signet's, only the chain differs. Note
        // Spark's SDK hosts operators for mainnet only, so signet and
        // testnet require self-hosted operators (`spark-operator` lines).
        "signet" | "mutinynet" => Ok(spark::Network::Signet),
        "testnet" => Ok(spark::Network::Testnet),
        "regtest" => Ok(spark::Network::Regtest),
        other => error::bail!("spark network `{other}` not supported"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spark_networks_parse_including_mutinynet() {
        assert!(matches!(
            parse_spark_network("mainnet").unwrap(),
            spark::Network::Mainnet
        ));
        assert!(matches!(
            parse_spark_network("bitcoin").unwrap(),
            spark::Network::Mainnet
        ));
        // mutinynet is a signet: same params, fast blocks.
        assert!(matches!(
            parse_spark_network("mutinynet").unwrap(),
            spark::Network::Signet
        ));
        assert!(matches!(
            parse_spark_network("signet").unwrap(),
            spark::Network::Signet
        ));
        assert!(matches!(
            parse_spark_network("testnet").unwrap(),
            spark::Network::Testnet
        ));
        assert!(matches!(
            parse_spark_network("regtest").unwrap(),
            spark::Network::Regtest
        ));
        assert!(parse_spark_network("nonsense").is_err());
    }
}

async fn run_httpd(lampod: Arc<LampoDaemon>) -> error::Result<()> {
    let url = format!("{}:{}", lampod.conf().api_host, lampod.conf().api_port);
    let host = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(&url)
        .to_string();
    tokio::spawn(lampo_httpd::run(lampod, host, url));
    Ok(())
}
