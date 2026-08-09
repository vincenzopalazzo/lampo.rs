//! Daemon settings, read from the node's own `lampo.conf`.
//!
//! There is deliberately no second config file and no TOML parser: the
//! swap keys live next to the lampo ones, in the same CLN-style
//! format, and are read through `LampoConf::get_value`.
//!
//! ```text
//! # lampo.conf
//! network=regtest
//! spark-network=regtest
//! swap-quote-expiry-secs=45
//! swap-htlc-expiry-secs=3600
//! swap-api-addr=127.0.0.1:9736
//! ```
use std::path::PathBuf;

use lampo_common::conf::LampoConf;
use lampo_common::error;

#[derive(Debug, Clone)]
pub struct Settings {
    /// `mainnet` or `regtest`.
    pub spark_network: String,
    /// Path to the 32 byte hex seed of the Spark wallet. Created on
    /// first start when missing.
    pub spark_seed_file: PathBuf,
    /// How long a Direction A quote waits for the Spark HTLC. Bounded
    /// by LDK reaping the fetched invoice after roughly a minute.
    pub quote_expiry_secs: u64,
    /// Lifetime of the Spark HTLCs the daemon creates.
    pub spark_htlc_expiry_secs: u64,
    /// Address of the daemon's own swap API.
    pub api_addr: String,
}

impl Settings {
    pub fn from_lampo_conf(conf: &LampoConf) -> error::Result<Self> {
        let swapd_dir = PathBuf::from(conf.path()).join("swapd");
        let spark_network = value(conf, "spark-network")?.unwrap_or_else(|| conf_network(conf));
        Ok(Self {
            spark_network,
            spark_seed_file: value(conf, "spark-seed-file")?
                .map(PathBuf::from)
                .unwrap_or_else(|| swapd_dir.join("spark.seed")),
            quote_expiry_secs: parsed(conf, "swap-quote-expiry-secs")?.unwrap_or(45),
            spark_htlc_expiry_secs: parsed(conf, "swap-htlc-expiry-secs")?.unwrap_or(3600),
            api_addr: value(conf, "swap-api-addr")?.unwrap_or_else(|| "127.0.0.1:9736".to_owned()),
        })
    }

    pub fn swapd_dir(conf: &LampoConf) -> PathBuf {
        PathBuf::from(conf.path()).join("swapd")
    }
}

/// Spark speaks `mainnet`/`regtest`; map the node's network onto that
/// when `spark-network` is not set explicitly.
fn conf_network(conf: &LampoConf) -> String {
    match conf.network.to_string().as_str() {
        "bitcoin" => "mainnet".to_owned(),
        other => other.to_owned(),
    }
}

fn value(conf: &LampoConf, key: &str) -> error::Result<Option<String>> {
    Ok(conf
        .get_value(key)
        .map_err(|err| error::anyhow!("reading `{key}` from lampo.conf: {err}"))?
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty()))
}

fn parsed(conf: &LampoConf, key: &str) -> error::Result<Option<u64>> {
    let Some(raw) = value(conf, key)? else {
        return Ok(None);
    };
    let parsed = raw
        .parse::<u64>()
        .map_err(|err| error::anyhow!("`{key}` must be a number, found `{raw}`: {err}"))?;
    Ok(Some(parsed))
}
