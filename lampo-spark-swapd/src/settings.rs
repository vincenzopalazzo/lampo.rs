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
    /// Spark signing operators to talk to. Empty means the defaults
    /// baked into the SDK, which are Lightspark's hosted ones; a local
    /// regtest stack must be named explicitly.
    pub spark_operators: Vec<OperatorEndpoint>,
}

/// One locally configured signing operator.
#[derive(Debug, Clone)]
pub struct OperatorEndpoint {
    pub id: usize,
    /// FROST identifier, 32 byte hex. Operator `n` uses `n + 1`.
    pub identifier: String,
    pub address: String,
    pub identity_public_key: String,
    /// PEM of the operator's self signed certificate, when it is not
    /// signed by a CA the host already trusts.
    pub ca_cert: Option<Vec<u8>>,
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
            spark_operators: operators(conf)?,
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

/// Read repeated `spark-operator=` lines. Each is
/// `<id>|<address>|<identity pubkey>[|<ca cert path>]`, for example
///
/// ```text
/// spark-operator=0|https://localhost:8535|0322ca...|/tmp/spark/server_0.crt
/// ```
///
/// The FROST identifier is derived from the id, matching how the
/// operators number themselves.
fn operators(conf: &LampoConf) -> error::Result<Vec<OperatorEndpoint>> {
    let Some(lines) = conf.get_values("spark-operator") else {
        return Ok(Vec::new());
    };
    let mut operators = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('|').map(|part| part.trim()).collect();
        if parts.len() < 3 {
            error::bail!(
                "`spark-operator` must be `<id>|<address>|<identity pubkey>[|<ca cert path>]`, found `{line}`"
            );
        }
        let id: usize = parts[0]
            .parse()
            .map_err(|err| error::anyhow!("operator id `{}` is not a number: {err}", parts[0]))?;
        let ca_cert =
            match parts.get(3) {
                Some(path) if !path.is_empty() => Some(std::fs::read(path).map_err(|err| {
                    error::anyhow!("cannot read the operator cert `{path}`: {err}")
                })?),
                _ => None,
            };
        operators.push(OperatorEndpoint {
            id,
            identifier: format!("{:064x}", id + 1),
            address: parts[1].to_owned(),
            identity_public_key: parts[2].to_owned(),
            ca_cert,
        });
    }
    Ok(operators)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a real LampoConf from a lampo.conf on disk, the same way
    /// the daemon does, so the parser is tested end to end.
    fn conf_with(lines: &str) -> LampoConf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "swapd-settings-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lampo.conf"),
            format!("network=regtest\nport=19735\n{lines}\n"),
        )
        .unwrap();
        LampoConf::try_from(dir.to_str().unwrap().to_owned()).unwrap()
    }

    #[test]
    fn no_operator_lines_mean_sdk_defaults() {
        let settings = Settings::from_lampo_conf(&conf_with("")).unwrap();
        assert!(settings.spark_operators.is_empty());
    }

    #[test]
    fn operator_lines_parse_in_order() {
        let settings = Settings::from_lampo_conf(&conf_with(
            "spark-operator=0|https://localhost:8535|02aa\n\
             spark-operator=1|https://localhost:8536|02bb",
        ))
        .unwrap();
        assert_eq!(settings.spark_operators.len(), 2);
        let first = &settings.spark_operators[0];
        assert_eq!(first.id, 0);
        assert_eq!(first.address, "https://localhost:8535");
        assert_eq!(first.identity_public_key, "02aa");
        // operator n signs as FROST identifier n + 1
        assert!(first.identifier.ends_with('1'));
        assert_eq!(first.identifier.len(), 64);
        assert!(first.ca_cert.is_none());
    }

    #[test]
    fn a_cert_path_is_read_into_bytes() {
        let cert = std::env::temp_dir().join(format!("swapd-cert-{}", std::process::id()));
        std::fs::write(&cert, b"PEMISH").unwrap();
        let settings = Settings::from_lampo_conf(&conf_with(&format!(
            "spark-operator=0|https://localhost:8535|02aa|{}",
            cert.display()
        )))
        .unwrap();
        assert_eq!(
            settings.spark_operators[0].ca_cert.as_deref(),
            Some(b"PEMISH".as_slice())
        );
    }

    #[test]
    fn malformed_operator_lines_are_refused_loudly() {
        // Too few fields must fail, not silently configure half an
        // operator pool that then signs with the wrong quorum.
        assert!(Settings::from_lampo_conf(&conf_with("spark-operator=0|onlyaddress")).is_err());
        // and so must a non numeric id
        assert!(Settings::from_lampo_conf(&conf_with(
            "spark-operator=zero|https://localhost:8535|02aa"
        ))
        .is_err());
        // and a cert path that does not exist
        assert!(Settings::from_lampo_conf(&conf_with(
            "spark-operator=0|https://localhost:8535|02aa|/definitely/not/here.crt"
        ))
        .is_err());
    }

    #[test]
    fn swap_settings_read_from_the_same_file() {
        let settings = Settings::from_lampo_conf(&conf_with(
            "swap-quote-expiry-secs=30\nswap-htlc-expiry-secs=600\nswap-api-addr=0.0.0.0:9999",
        ))
        .unwrap();
        assert_eq!(settings.quote_expiry_secs, 30);
        assert_eq!(settings.spark_htlc_expiry_secs, 600);
        assert_eq!(settings.api_addr, "0.0.0.0:9999");
        // and the network falls back to the node's, mapped for spark
        assert_eq!(settings.spark_network, "regtest");
    }

    #[test]
    fn a_non_numeric_expiry_is_an_error() {
        assert!(Settings::from_lampo_conf(&conf_with("swap-quote-expiry-secs=soon")).is_err());
    }
}
