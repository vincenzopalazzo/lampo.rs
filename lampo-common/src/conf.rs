use std::str::FromStr;

use bitcoin::absolute::Height;
use clightningrpc_conf::{CLNConf, SyncCLNConf};

use crate::extension::PersistentPeer;

pub use bitcoin::Network;
pub use lightning::util::config::UserConfig;

#[derive(Clone, Debug)]
pub struct LampoConf {
    pub inner: Option<CLNConf>,
    pub network: Network,
    pub ldk_conf: UserConfig,
    pub port: u64,
    pub root_path: String,
    /// The backend implementation
    pub node: String,
    pub core_url: Option<String>,
    pub core_user: Option<String>,
    pub core_pass: Option<String>,
    pub private_key: Option<String>,
    pub channels_keys: Option<String>,
    pub log_file: Option<String>,
    pub log_level: String,
    pub alias: Option<String>,
    pub announce_addr: Option<String>,
    pub api_host: String,
    pub api_port: u64,
    /// Serve the LND-compatible API instead of lampo-httpd.
    pub lnd: Option<bool>,
    /// Additional DNS names or IP addresses for the LND REST TLS certificate.
    pub lnd_tls_sans: Vec<String>,
    pub reindex: Option<Height>,
    pub dev_sync: Option<bool>,
    /// Allow the on-chain wallet to scan in parallel with the LDK chain
    /// listener sync. Defaults to `false`: the wallet waits for listeners to
    /// catch up first, so the two pipelines don't compete for the same RPC.
    pub wallet_sync_parallel: Option<bool>,
    /// Chain sync strategy: `"unified"` (default) drives the on-chain wallet
    /// through the same `synchronize_listeners` pass as the LDK listeners;
    /// `"legacy"` keeps the wallet on its standalone BDK Emitter scan.
    pub sync_mode: Option<String>,
    /// Fast-sync an empty wallet by jumping its checkpoint to the chain tip
    /// instead of scanning from genesis. Defaults to `true` and only applies
    /// to a fresh wallet (no UTXOs to miss); set `false` to force a full scan.
    pub fast_sync: Option<bool>,
    /// Async payments role. Unset (the default) leaves async payments off:
    /// this node does not process static-invoice onion messages or hold HTLCs.
    /// `client` holds outbound HTLCs at the next hop so the node can go
    /// offline after sending; `server` holds HTLCs and serves static invoices
    /// on behalf of often-offline recipients.
    pub async_payments_role: Option<String>,
    /// Hex-encoded `Vec<BlindedMessagePath>` obtained out-of-band from a
    /// static invoice server. Configures this node as an often-offline async
    /// recipient: `offer` then returns the async receive offer.
    pub async_invoice_server_paths: Option<String>,
    /// Optional token required by `asyncinvoicepaths` and
    /// `setasyncinvoicepaths`. Other RPCs stay unauthenticated (localhost
    /// plus the HTTP DNS-rebinding guard).
    pub api_token: Option<String>,
    /// Fixed ISO 4217 rates for BOLT 12 currency-denominated offers.
    ///
    /// Each entry is `CODE=msats_per_minor_unit` (USD cents, JPY yen).
    /// Comma-separated. Empty means currency offers are rejected.
    pub currency_rates: Vec<(String, u64)>,
    /// Symmetric tolerance, in basis points, around each configured rate.
    /// One basis point is 0.01%. Default is 100 (1%).
    pub currency_tolerance_bps: u16,
    /// ACINQ Phoenix LSP this node is a client of, as `NODE_ID@HOST:PORT`.
    /// The literal `default` selects ACINQ's testnet3 or mainnet node for
    /// the configured network. Unset (the default) leaves the Phoenix
    /// handler installed but idle: no feature bits, every message dropped.
    pub phoenix_lsp: Option<String>,
    /// Inbound liquidity to request from the Phoenix LSP when a payment does
    /// not fit, in sat. Unset disables the liquidity policy: every
    /// on-the-fly funding proposal is rejected. Decision only, nothing is
    /// purchased yet.
    pub phoenix_auto_liquidity: Option<u64>,
    /// Maximum fee credit the Phoenix LSP may hold for this node, in sat.
    /// Default 0: a payment too small to pay its own funding fee is rejected.
    pub phoenix_max_fee_credit: u64,
    /// Maximum funding fee (mining plus service) relative to the amount
    /// received, in basis points. Default 250 (2.5%).
    pub phoenix_max_relative_fee_bps: u16,
    /// Maximum mining fee of a funding transaction, in sat. Unset rejects
    /// every on-the-fly funding proposal once `phoenix-auto-liquidity` is set.
    pub phoenix_max_mining_fee: Option<u64>,
}

/// ACINQ's testnet3 Phoenix LSP, selected by `phoenix-lsp=default`.
pub const PHOENIX_LSP_TESTNET3: &str =
    "03933884aaf1d6b108397e5efe5c86bcf2d8ca8d2f700eda99db9214fc2712b134@13.248.222.197:9735";
/// ACINQ's mainnet Phoenix LSP, selected by `phoenix-lsp=default`.
pub const PHOENIX_LSP_MAINNET: &str =
    "03864ef025fde8fb587d989186ce6a4a186895ee44a926bfc370e2c366597a3f8f@3.33.236.230:9735";

/// A parsed `phoenix-lsp` value: the LSP node id and where to dial it.
pub type PhoenixLspPeer = PersistentPeer;

impl LampoConf {
    /// Resolve the default lampo root path.
    ///
    /// Resolution order: `$LAMPO_HOME`, then `$HOME/.lampo`, then
    /// `./.lampo` as a last-resort fallback. Never panics: a daemon
    /// started from a minimal systemd unit or a container may not have
    /// a determinable home directory.
    /// (uses the deprecated `std::env::home_dir()` to avoid a dependency on dirs)
    pub fn default_root_path() -> String {
        if let Ok(path) = std::env::var("LAMPO_HOME") {
            path
        } else {
            #[allow(deprecated)]
            match std::env::home_dir() {
                Some(path) => format!("{}/.lampo", path.to_string_lossy()),
                None => "./.lampo".to_owned(),
            }
        }
    }
}

impl Default for LampoConf {
    fn default() -> Self {
        // default path for the configuration file (never panics, see
        // `LampoConf::default_root_path`)
        let lampo_home = Self::default_root_path();
        Self {
            inner: None,
            // default network is testnet
            network: Network::Testnet,
            ldk_conf: UserConfig::default(),
            // default port is 19735 for testnet
            port: 19735,
            root_path: lampo_home,
            node: "core".to_owned(),
            core_url: None,
            core_user: None,
            core_pass: None,
            private_key: None,
            channels_keys: None,
            log_level: "info".to_string(),
            log_file: None,
            alias: None,
            announce_addr: None,
            api_host: "127.0.0.1".to_owned(),
            api_port: 7878,
            lnd: None,
            lnd_tls_sans: Vec::new(),
            reindex: None,
            dev_sync: None,
            wallet_sync_parallel: None,
            sync_mode: None,
            fast_sync: None,
            async_payments_role: None,
            async_invoice_server_paths: None,
            api_token: None,
            currency_rates: Vec::new(),
            currency_tolerance_bps: 100,
            phoenix_lsp: None,
            phoenix_auto_liquidity: None,
            phoenix_max_fee_credit: 0,
            phoenix_max_relative_fee_bps: 250,
            phoenix_max_mining_fee: None,
        }
    }
}

impl LampoConf {
    pub fn prepare_dirs(&self) -> Result<(), anyhow::Error> {
        Self::prepare_directories(&self.root_path, Some(self.network))
    }

    pub fn prepare_directories(
        root_path: &str,
        network: Option<Network>,
    ) -> Result<(), anyhow::Error> {
        let root_path = Self::normalize_root_dir(root_path, network.unwrap_or(Network::Testnet));
        // make sure that the data-dir exist
        if !std::path::Path::new(&root_path).exists() {
            log::info!("Creating root dir at `{}`", root_path);
            std::fs::create_dir(root_path.clone())?;
        }

        if let Some(network) = network {
            let network_path = format!("{root_path}/{network}");
            if !std::path::Path::new(&network_path).exists() {
                log::info!("Creating network directory at `{network_path}`");
                std::fs::create_dir(network_path)?;
            }
        }
        Ok(())
    }
    // Sometimes the root path is given already with the network
    // e.g: when we read the datadir from the cli args we do not have
    // any way to get the network from the string (because it contains the root)
    #[inline(always)]
    pub fn normalize_root_dir(root_path: &str, network: Network) -> String {
        let suffix_with_slash = format!("/{network}/");
        let suffix_without_slash = format!("/{network}");

        let root = if root_path.ends_with(&suffix_with_slash)
            || root_path.ends_with(&suffix_without_slash)
        {
            root_path
                .trim_end()
                .strip_suffix(&suffix_with_slash)
                // SAFETY: we make a check before inside the if condition
                // so it is safe unwrap here otherwise we are hiding a bug
                // and we must crash.
                .or_else(|| root_path.strip_suffix(&suffix_without_slash))
                .unwrap_or_else(|| panic!("path: {root_path} - network: {network}"))
                .to_owned()
        } else {
            root_path.to_owned()
        };
        log::trace!("normalize root path {root} for network {network}");
        root
    }

    pub fn new(
        path: Option<String>,
        network: Option<Network>,
        port: Option<u64>,
    ) -> Result<Self, anyhow::Error> {
        let mut conf = Self::default();
        conf.network = network.unwrap_or(conf.network);
        conf.port = port.unwrap_or(conf.port);
        conf.root_path = path.clone().unwrap_or(conf.root_path);
        Self::prepare_directories(&conf.root_path, Some(conf.network))?;
        let input_path = path;
        let path = Self::normalize_root_dir(&conf.root_path, conf.network);
        conf.root_path = path.clone();

        let lampo_file = format!("{}/lampo.conf", conf.path());

        if std::fs::File::open(lampo_file.clone()).is_ok() {
            let mut conf = Self::try_from(conf.path())?;
            conf.network = network.unwrap_or(conf.network);
            conf.port = port.unwrap_or(conf.port);
            conf.root_path = input_path.unwrap_or(conf.root_path);
            return Ok(conf);
        }

        Ok(conf)
    }
}

impl TryFrom<String> for LampoConf {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let path = format!("{value}/lampo.conf");
        // Check for double slashes
        let path = path.replace("//", "/");

        // If lampo.conf doesn't exist, return the default configuration
        if !std::path::Path::new(&path).exists() {
            anyhow::bail!("Configuration file not found at `{path}`");
        }

        let mut conf = CLNConf::new(path, false);
        conf.parse()
            .map_err(|err| anyhow::anyhow!("{}", err.cause))?;

        let Some(network) = conf
            .get_conf("network")
            .map_err(|err| anyhow::anyhow!("{err}"))?
        else {
            anyhow::bail!("Network inside the configuration file missed");
        };

        let Some(port) = conf
            .get_conf("port")
            .map_err(|err| anyhow::anyhow!("{err}"))?
        else {
            anyhow::bail!("Port need to be specified inside the file");
        };

        let node = conf
            .get_conf("backend")
            .map_err(|err| anyhow::anyhow!("{err}"))?
            .unwrap_or("nakamoto".to_owned());
        // Strip the value of whitespace
        let node = node.to_trimmed();

        let mut core_url = None;
        let mut core_user = None;
        let mut core_pass = None;
        if node == "core" {
            core_url = conf
                .get_conf("core-url")
                .map_err(|err| anyhow::anyhow!("{err}"))?;
            // If the value isn't none, strip the value of whitespace
            core_url = core_url.map(|url| url.to_trimmed());

            core_user = conf
                .get_conf("core-user")
                .map_err(|err| anyhow::anyhow!("{err}"))?;
            core_user = core_user.map(|user| user.to_trimmed());

            core_pass = conf
                .get_conf("core-pass")
                .map_err(|err| anyhow::anyhow!("{err}"))?;
            core_pass = core_pass.map(|pass| pass.to_trimmed());
        }

        let reindex: Option<String> = conf
            .get_conf("reindex")
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let reindex = if let Some(reindex) = reindex {
            let reindex = Height::from_str(&reindex)?;
            Some(reindex)
        } else {
            None
        };
        // Dev options
        #[allow(unused_mut, unused_assignments)]
        let mut private_key: Option<String> = None;
        #[allow(unused_mut, unused_assignments)]
        let mut channels_keys: Option<String> = None;

        #[cfg(debug_assertions)]
        {
            private_key = conf
                .get_conf("dev-private-key")
                .map_err(|err| anyhow::anyhow!("{err}"))?;

            channels_keys = conf
                .get_conf("dev-force-channel-secrets")
                .map_err(|err| anyhow::anyhow!("{err}"))?;
        }

        let network = Network::from_str(&network)?;
        let root_path = Self::normalize_root_dir(&value, network);
        let log_level = conf.get_conf("log-level");
        let level = match log_level {
            Ok(Some(level)) => level,
            _ => "info".to_string(),
        };
        let log_file = conf.get_conf("log-file").unwrap_or(None);
        let alias = conf.get_conf("alias").unwrap_or(None);
        let announce_addr = conf.get_conf("announce-addr").unwrap_or(None);
        let api_host = conf.get_conf("api-host").unwrap_or(None);
        let api_port = conf.get_conf("api-port").unwrap_or(None);
        let api_host = api_host.unwrap_or("http://127.0.0.1".to_owned());
        let api_port: u64 = api_port.unwrap_or("7979".to_owned()).parse()?;

        let lnd = conf
            .get_conf("lnd")
            .unwrap_or(None)
            .map(|s| s.to_lowercase() == "true" || s == "1");
        let lnd_tls_sans = conf.get_confs("lnd-tls-san");

        // Parse dev_sync field - defaults to None (false)
        let dev_sync = conf
            .get_conf("dev-sync")
            .unwrap_or(None)
            .map(|s| s.to_lowercase() == "true" || s == "1");
        // Parse wallet_sync_parallel field - defaults to None (false)
        let wallet_sync_parallel = conf
            .get_conf("wallet-sync-parallel")
            .unwrap_or(None)
            .map(|s| s.to_lowercase() == "true" || s == "1");
        // Parse sync_mode field - defaults to None ("unified")
        let sync_mode = conf.get_conf("sync-mode").unwrap_or(None);
        // Parse fast_sync field - defaults to None (true)
        let fast_sync = conf
            .get_conf("fast-sync")
            .unwrap_or(None)
            .map(|s| s.to_lowercase() == "true" || s == "1");
        let async_payments_role = conf.get_conf("async-payments-role").unwrap_or(None);
        if let Some(role) = async_payments_role.as_deref() {
            if role != "client" && role != "server" {
                anyhow::bail!(
                    "invalid async-payments-role `{role}`: expected `client` or `server`"
                );
            }
        }
        let async_invoice_server_paths =
            conf.get_conf("async-invoice-server-paths").unwrap_or(None);
        let api_token = conf
            .get_conf("api-token")
            .unwrap_or(None)
            .and_then(|token| {
                let token = token.trim().to_owned();
                if token.is_empty() {
                    None
                } else {
                    Some(token)
                }
            });
        let currency_rates_raw = conf.get_conf("currency-rates").unwrap_or(None);
        let currency_rates = parse_currency_rates(currency_rates_raw.as_deref())?;
        let currency_tolerance_bps = conf
            .get_conf("currency-tolerance-bps")
            .unwrap_or(None)
            .map(|raw| {
                raw.parse::<u16>()
                    .map_err(|_| anyhow::anyhow!("invalid currency-tolerance-bps `{raw}`"))
            })
            .transpose()?
            .unwrap_or(100);
        if currency_tolerance_bps >= 10_000 {
            anyhow::bail!(
                "currency-tolerance-bps `{currency_tolerance_bps}` must be below 10000 (100%)"
            );
        }
        let phoenix_lsp = conf
            .get_conf("phoenix-lsp")
            .unwrap_or(None)
            .map(|raw| resolve_phoenix_lsp(&raw, network))
            .transpose()?;
        let phoenix_auto_liquidity = parse_u64_key(&conf, "phoenix-auto-liquidity")?;
        let phoenix_max_fee_credit = parse_u64_key(&conf, "phoenix-max-fee-credit")?.unwrap_or(0);
        let phoenix_max_relative_fee_bps = parse_u64_key(&conf, "phoenix-max-relative-fee-bps")?
            .map(|bps| {
                u16::try_from(bps)
                    .ok()
                    .filter(|bps| *bps < 10_000)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "phoenix-max-relative-fee-bps `{bps}` must be below 10000 (100%)"
                        )
                    })
            })
            .transpose()?
            .unwrap_or(250);
        let phoenix_max_mining_fee = parse_u64_key(&conf, "phoenix-max-mining-fee")?;
        Ok(Self {
            inner: Some(conf),
            root_path,
            network,
            ldk_conf: UserConfig::default(),
            port: u64::from_str(&port)?,
            node,
            core_url,
            core_user,
            core_pass,
            private_key,
            channels_keys,
            log_file,
            log_level: level,
            alias,
            announce_addr,
            api_host,
            api_port,
            lnd,
            lnd_tls_sans,
            reindex,
            dev_sync,
            wallet_sync_parallel,
            sync_mode,
            fast_sync,
            async_payments_role,
            async_invoice_server_paths,
            api_token,
            currency_rates,
            currency_tolerance_bps,
            phoenix_lsp,
            phoenix_auto_liquidity,
            phoenix_max_fee_credit,
            phoenix_max_relative_fee_bps,
            phoenix_max_mining_fee,
        })
    }
}

/// Read an optional unsigned integer key, in the unit the key documents.
fn parse_u64_key(conf: &CLNConf, key: &str) -> anyhow::Result<Option<u64>> {
    conf.get_conf(key)
        .unwrap_or(None)
        .map(|raw| {
            raw.trim()
                .parse::<u64>()
                .map_err(|_| anyhow::anyhow!("invalid {key} `{raw}`: expected an integer"))
        })
        .transpose()
}

/// Turn a raw `phoenix-lsp` value into its canonical `NODE_ID@HOST:PORT`
/// form, expanding `default` to ACINQ's node for `network`.
fn resolve_phoenix_lsp(raw: &str, network: Network) -> anyhow::Result<String> {
    let raw = raw.trim();
    let raw = if raw == "default" {
        match network {
            Network::Bitcoin => PHOENIX_LSP_MAINNET,
            Network::Testnet => PHOENIX_LSP_TESTNET3,
            _ => anyhow::bail!(
                "phoenix-lsp=default has no ACINQ node on `{network}`: set NODE_ID@HOST:PORT explicitly"
            ),
        }
    } else {
        raw
    };
    Ok(PhoenixLspPeer::from_str(raw)?.to_string())
}

fn parse_currency_rates(raw: Option<&str>) -> anyhow::Result<Vec<(String, u64)>> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(Vec::new());
    };
    let mut rates = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (code, rate) = entry.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("invalid currency-rates entry `{entry}`: expected CODE=msats")
        })?;
        let code = code.trim().to_ascii_uppercase();
        if code.len() != 3 || !code.bytes().all(|byte| byte.is_ascii_uppercase()) {
            anyhow::bail!("invalid currency code `{code}`: expected 3 ASCII letters");
        }
        let rate = rate.trim().parse::<u64>().map_err(|_| {
            anyhow::anyhow!("invalid currency rate for `{code}`: expected a u64 millisatoshi rate")
        })?;
        if rate == 0 {
            anyhow::bail!("currency rate for `{code}` must be non-zero");
        }
        if rates.iter().any(|(existing, _)| existing == &code) {
            anyhow::bail!("duplicate currency rate for `{code}`");
        }
        rates.push((code, rate));
    }
    Ok(rates)
}

impl LampoConf {
    pub fn path(&self) -> String {
        format!("{}/{}", self.root_path, self.network)
    }

    /// Whether this config opts into async payments at startup.
    ///
    /// A later `setasyncinvoicepaths` call is a separate runtime opt-in.
    /// Hold flags and the onion-message handler stay off until one of
    /// those is set.
    pub fn async_payments_configured(&self) -> bool {
        self.async_payments_role.is_some() || self.async_invoice_server_paths.is_some()
    }

    /// The LDK config adjusted for the configured async payments role.
    ///
    /// Defaults keep `enable_htlc_hold` and `hold_outbound_htlcs_at_next_hop`
    /// off. A `server` holds HTLCs for often-offline recipients and accepts
    /// forwards to private channels (the recipient's channel to its server is
    /// typically unannounced); a `client` asks its next hop to hold outbound
    /// HTLCs so it can go offline after sending.
    pub fn ldk_conf_with_async_role(&self) -> UserConfig {
        let mut conf = self.ldk_conf.clone();
        match self.async_payments_role.as_deref() {
            Some("server") => {
                conf.enable_htlc_hold = true;
                conf.accept_forwards_to_priv_channels = true;
            }
            Some("client") => {
                conf.hold_outbound_htlcs_at_next_hop = true;
            }
            _ => {}
        }
        conf
    }

    /// The configured Phoenix LSP, if any. The stored string was validated
    /// at parse time, so a failure here means it was edited afterwards.
    pub fn phoenix_lsp_peer(&self) -> anyhow::Result<Option<PhoenixLspPeer>> {
        self.phoenix_lsp
            .as_deref()
            .map(PhoenixLspPeer::from_str)
            .transpose()
    }

    pub fn get_values(&self, key: &str) -> Option<Vec<String>> {
        self.inner.as_ref().map(|conf| conf.get_confs(key))
    }

    pub fn get_value(&self, key: &str) -> Result<Option<String>, anyhow::Error> {
        let conf = self
            .inner
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Lampo configuration was not loaded"))?;

        let Some(value) = conf.get_conf(key).map_err(|err| anyhow::anyhow!("{err}"))? else {
            return Ok(None);
        };
        Ok(Some(value))
    }

    pub fn set_network(&mut self, network: &str) -> anyhow::Result<()> {
        self.network = Network::from_str(network)?;
        Ok(())
    }
}

// A trait to trim a String
trait TrimmedString {
    fn to_trimmed(self) -> String;
}

impl TrimmedString for String {
    fn to_trimmed(self) -> String {
        self.trim().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (bug 3): `LampoConf::default()` must not panic when the
    /// home directory cannot be determined; it falls back to `$LAMPO_HOME`
    /// and finally to `./.lampo`.
    ///
    /// NOTE: before the fix this panicked with
    /// `expect("Impossible to get the home directory")` only where
    /// `std::env::home_dir()` has no passwd fallback (e.g. distroless
    /// containers); on developer machines the panic could not be forced
    /// via env manipulation, so this asserts the new fallback behavior.
    #[test]
    fn lampo_conf_default_falls_back_gracefully() {
        std::env::remove_var("LAMPO_HOME");
        std::env::remove_var("HOME");
        // No HOME and no LAMPO_HOME: must not panic. Where a passwd
        // fallback exists the real home is used; otherwise we get the
        // `./.lampo` relative fallback. Either way the path is usable.
        let conf = LampoConf::default();
        assert!(!conf.root_path.is_empty());
        // LAMPO_HOME always wins.
        std::env::set_var("LAMPO_HOME", "/tmp/lampo-home-fallback-test");
        let conf = LampoConf::default();
        assert_eq!(conf.root_path, "/tmp/lampo-home-fallback-test");
        std::env::remove_var("LAMPO_HOME");
    }

    #[test]
    fn async_payments_disabled_by_default() {
        let conf = LampoConf::default();
        assert!(!conf.async_payments_configured());
        assert!(conf.async_payments_role.is_none());
        assert!(conf.async_invoice_server_paths.is_none());

        let ldk = conf.ldk_conf_with_async_role();
        assert!(!ldk.enable_htlc_hold);
        assert!(!ldk.hold_outbound_htlcs_at_next_hop);
    }

    #[test]
    fn async_payments_role_enables_hold_flags() {
        let mut client = LampoConf::default();
        client.async_payments_role = Some("client".to_owned());
        assert!(client.async_payments_configured());
        let client_ldk = client.ldk_conf_with_async_role();
        assert!(client_ldk.hold_outbound_htlcs_at_next_hop);
        assert!(!client_ldk.enable_htlc_hold);

        let mut server = LampoConf::default();
        server.async_payments_role = Some("server".to_owned());
        let server_ldk = server.ldk_conf_with_async_role();
        assert!(server_ldk.enable_htlc_hold);
        assert!(server_ldk.accept_forwards_to_priv_channels);
    }

    #[test]
    fn phoenix_lsp_default_follows_the_network() {
        assert_eq!(
            resolve_phoenix_lsp("default", Network::Bitcoin).unwrap(),
            PHOENIX_LSP_MAINNET
        );
        assert_eq!(
            resolve_phoenix_lsp(" default ", Network::Testnet).unwrap(),
            PHOENIX_LSP_TESTNET3
        );
        assert!(resolve_phoenix_lsp("default", Network::Regtest).is_err());
        assert!(resolve_phoenix_lsp("default", Network::Signet).is_err());
    }

    #[test]
    fn phoenix_lsp_peer_is_parsed_from_the_config() {
        let mut conf = LampoConf::default();
        assert!(conf.phoenix_lsp_peer().unwrap().is_none());
        conf.phoenix_lsp = Some(PHOENIX_LSP_MAINNET.to_owned());
        assert_eq!(
            conf.phoenix_lsp_peer().unwrap().unwrap().node_id,
            PhoenixLspPeer::from_str(PHOENIX_LSP_MAINNET)
                .unwrap()
                .node_id
        );
    }
}
