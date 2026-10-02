//! The `phoenix-*` keys of `lampo.conf`, read by this crate from the
//! daemon's configuration so `lampo-common` knows nothing about Phoenix.

use std::str::FromStr;

use lampo_common::bitcoin::Network;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::extension::PersistentPeer;

/// ACINQ's testnet3 Phoenix LSP, selected by `phoenix-lsp=default`.
pub const PHOENIX_LSP_TESTNET3: &str =
    "03933884aaf1d6b108397e5efe5c86bcf2d8ca8d2f700eda99db9214fc2712b134@13.248.222.197:9735";
/// ACINQ's mainnet Phoenix LSP, selected by `phoenix-lsp=default`.
pub const PHOENIX_LSP_MAINNET: &str =
    "03864ef025fde8fb587d989186ce6a4a186895ee44a926bfc370e2c366597a3f8f@3.33.236.230:9735";

/// A parsed `phoenix-lsp` value: the LSP node id and where to dial it.
pub type PhoenixLspPeer = PersistentPeer;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoenixConf {
    /// `phoenix-lsp`: the LSP this node is a client of. `default` selects
    /// ACINQ's testnet3 or mainnet node for the configured network. Unset
    /// leaves the handler installed but idle: no feature bits, every
    /// message dropped.
    pub lsp: Option<PhoenixLspPeer>,
    /// `phoenix-auto-liquidity`: inbound liquidity to request when a payment
    /// does not fit, in sat. Unset disables the liquidity policy: every
    /// on-the-fly funding proposal is rejected. Decision only, nothing is
    /// purchased yet.
    pub auto_liquidity_sat: Option<u64>,
    /// `phoenix-max-fee-credit`: the most fee credit the LSP may hold for
    /// this node, in sat. Default 0: a payment too small to pay its own
    /// funding fee is rejected.
    pub max_fee_credit_sat: u64,
    /// `phoenix-max-relative-fee-bps`: maximum funding fee (mining plus
    /// service) relative to the amount received, in basis points. Default
    /// 250 (2.5%).
    pub max_relative_fee_bps: u16,
    /// `phoenix-max-mining-fee`: maximum mining fee of a funding
    /// transaction, in sat. Unset rejects every on-the-fly funding proposal
    /// once `phoenix-auto-liquidity` is set.
    pub max_mining_fee_sat: Option<u64>,
}

impl Default for PhoenixConf {
    fn default() -> Self {
        Self {
            lsp: None,
            auto_liquidity_sat: None,
            max_fee_credit_sat: 0,
            max_relative_fee_bps: 250,
            max_mining_fee_sat: None,
        }
    }
}

impl PhoenixConf {
    /// Read the `phoenix-*` keys out of `conf`; missing keys take their
    /// defaults.
    pub fn from_lampo_conf(conf: &LampoConf) -> error::Result<Self> {
        let defaults = Self::default();
        let lsp = conf
            .get_extension_value("phoenix-lsp")?
            .map(|raw| resolve_lsp(&raw, conf.network))
            .transpose()?;
        let max_relative_fee_bps = parse_u64_key(conf, "phoenix-max-relative-fee-bps")?
            .map(|bps| {
                u16::try_from(bps)
                    .ok()
                    .filter(|bps| *bps < 10_000)
                    .ok_or_else(|| {
                        error::anyhow!(
                            "phoenix-max-relative-fee-bps `{bps}` must be below 10000 (100%)"
                        )
                    })
            })
            .transpose()?
            .unwrap_or(defaults.max_relative_fee_bps);
        Ok(Self {
            lsp,
            auto_liquidity_sat: parse_u64_key(conf, "phoenix-auto-liquidity")?,
            max_fee_credit_sat: parse_u64_key(conf, "phoenix-max-fee-credit")?
                .unwrap_or(defaults.max_fee_credit_sat),
            max_relative_fee_bps,
            max_mining_fee_sat: parse_u64_key(conf, "phoenix-max-mining-fee")?,
        })
    }
}

/// Read an optional unsigned integer key, in the unit the key documents.
fn parse_u64_key(conf: &LampoConf, key: &str) -> error::Result<Option<u64>> {
    conf.get_extension_value(key)?
        .map(|raw| {
            raw.trim()
                .parse::<u64>()
                .map_err(|_| error::anyhow!("invalid {key} `{raw}`: expected an integer"))
        })
        .transpose()
}

/// Parse a raw `phoenix-lsp` value, expanding `default` to ACINQ's node
/// for `network`.
fn resolve_lsp(raw: &str, network: Network) -> error::Result<PhoenixLspPeer> {
    let raw = raw.trim();
    let raw = if raw == "default" {
        match network {
            Network::Bitcoin => PHOENIX_LSP_MAINNET,
            Network::Testnet => PHOENIX_LSP_TESTNET3,
            _ => error::bail!(
                "phoenix-lsp=default has no ACINQ node on `{network}`: set NODE_ID@HOST:PORT explicitly"
            ),
        }
    } else {
        raw
    };
    PhoenixLspPeer::from_str(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_follows_the_network() {
        assert_eq!(
            resolve_lsp("default", Network::Bitcoin)
                .unwrap()
                .to_string(),
            PHOENIX_LSP_MAINNET
        );
        assert_eq!(
            resolve_lsp(" default ", Network::Testnet)
                .unwrap()
                .to_string(),
            PHOENIX_LSP_TESTNET3
        );
        assert!(resolve_lsp("default", Network::Regtest).is_err());
        assert!(resolve_lsp("default", Network::Signet).is_err());
    }

    #[test]
    fn reads_its_keys_from_the_daemon_conf() {
        let mut conf = LampoConf::default();
        assert_eq!(
            PhoenixConf::from_lampo_conf(&conf).unwrap(),
            PhoenixConf::default()
        );
        conf.network = Network::Bitcoin;
        conf.set_extension_value("phoenix-lsp", "default").unwrap();
        conf.set_extension_value("phoenix-auto-liquidity", "2000000")
            .unwrap();
        conf.set_extension_value("phoenix-max-relative-fee-bps", "300")
            .unwrap();
        conf.set_extension_value("phoenix-max-mining-fee", "20000")
            .unwrap();
        let phoenix = PhoenixConf::from_lampo_conf(&conf).unwrap();
        assert_eq!(phoenix.lsp.unwrap().to_string(), PHOENIX_LSP_MAINNET);
        assert_eq!(phoenix.auto_liquidity_sat, Some(2_000_000));
        assert_eq!(phoenix.max_fee_credit_sat, 0);
        assert_eq!(phoenix.max_relative_fee_bps, 300);
        assert_eq!(phoenix.max_mining_fee_sat, Some(20_000));

        let mut bad = LampoConf::default();
        bad.set_extension_value("phoenix-max-relative-fee-bps", "10000")
            .unwrap();
        assert!(PhoenixConf::from_lampo_conf(&bad).is_err());
        let mut bad = LampoConf::default();
        bad.set_extension_value("phoenix-max-mining-fee", "lots")
            .unwrap();
        assert!(PhoenixConf::from_lampo_conf(&bad).is_err());
    }
}
