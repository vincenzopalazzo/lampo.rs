//! Cached backing store for LDK's [`FeeEstimator`].
//!
//! Mirrors ldk-node's `OnchainFeeEstimator`: async refresh into an `RwLock`,
//! sync reads on the HTLC path, same block targets / fallbacks / post-adjustments.

use std::collections::HashMap;
use std::sync::RwLock;

use lampo_common::backend::FeeEstimateMode;
use lampo_common::ldk::chain::chaininterface::{ConfirmationTarget, FEERATE_FLOOR_SATS_PER_KW};

/// How often we poll bitcoind. ldk-node default: 10 minutes.
pub const FEE_CACHE_REFRESH_SECS: u64 = 600;

/// Per-RPC timeout. ldk-node: `FEE_RATE_CACHE_UPDATE_TIMEOUT_SECS`.
pub const FEE_CACHE_UPDATE_TIMEOUT_SECS: u64 = 5;

/// ldk-node's regtest/signet fallback: 1 sat/vB == 250 sat/kW.
/// [`FeeCache::get`] still clamps to [`FEERATE_FLOOR_SATS_PER_KW`].
pub const RELAY_FALLBACK_SAT_PER_KW: u32 = 250;

/// Wallet + LDK fee targets. Same split as ldk-node: `ChannelFunding` /
/// `OnchainPayment` are not LDK `ConfirmationTarget`s.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum FeeTarget {
    OnchainPayment,
    ChannelFunding,
    Lightning(ConfirmationTarget),
}

impl From<ConfirmationTarget> for FeeTarget {
    fn from(value: ConfirmationTarget) -> Self {
        Self::Lightning(value)
    }
}

/// sat/vB → sat/kW. LDK documents this as `satoshis-per-byte * 250`.
#[cfg(test)]
pub fn sat_per_vb_to_kw(sat_vb: u32) -> u32 {
    sat_vb.saturating_mul(250)
}

/// Last-resort sat/kW when the cache is cold. Same numbers as ldk-node.
pub fn fallback_sat_per_kw(target: FeeTarget) -> u32 {
    match target {
        FeeTarget::OnchainPayment => 5000,
        FeeTarget::ChannelFunding => 1000,
        FeeTarget::Lightning(ldk_target) => match ldk_target {
            ConfirmationTarget::MaximumFeeEstimate => 8000,
            ConfirmationTarget::UrgentOnChainSweep => 5000,
            ConfirmationTarget::MinAllowedAnchorChannelRemoteFee
            | ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee => FEERATE_FLOOR_SATS_PER_KW,
            ConfirmationTarget::AnchorChannelFee => 500,
            ConfirmationTarget::NonAnchorChannelFee => 1000,
            ConfirmationTarget::ChannelCloseMinimum => 500,
            ConfirmationTarget::OutputSpendingFee => 1000,
        },
    }
}

/// Where the sat/kW for this target comes from. Matches ldk-node's bitcoind
/// `update_fee_rate_estimates` match.
pub enum FeeSource {
    MempoolMin,
    Blocks { blocks: u64, mode: FeeEstimateMode },
}

pub fn source_for_target(target: FeeTarget) -> FeeSource {
    match target {
        FeeTarget::OnchainPayment => FeeSource::Blocks {
            blocks: 6,
            mode: FeeEstimateMode::Economical,
        },
        FeeTarget::ChannelFunding => FeeSource::Blocks {
            blocks: 12,
            mode: FeeEstimateMode::Economical,
        },
        FeeTarget::Lightning(ConfirmationTarget::MinAllowedAnchorChannelRemoteFee) => {
            FeeSource::MempoolMin
        }
        FeeTarget::Lightning(ConfirmationTarget::MaximumFeeEstimate) => FeeSource::Blocks {
            blocks: 1,
            mode: FeeEstimateMode::Conservative,
        },
        FeeTarget::Lightning(ConfirmationTarget::UrgentOnChainSweep) => FeeSource::Blocks {
            blocks: 6,
            mode: FeeEstimateMode::Conservative,
        },
        FeeTarget::Lightning(ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee) => {
            FeeSource::Blocks {
                blocks: 144,
                mode: FeeEstimateMode::Economical,
            }
        }
        FeeTarget::Lightning(ConfirmationTarget::AnchorChannelFee) => FeeSource::Blocks {
            blocks: 1008,
            mode: FeeEstimateMode::Economical,
        },
        FeeTarget::Lightning(ConfirmationTarget::NonAnchorChannelFee) => FeeSource::Blocks {
            blocks: 12,
            mode: FeeEstimateMode::Economical,
        },
        FeeTarget::Lightning(ConfirmationTarget::ChannelCloseMinimum) => FeeSource::Blocks {
            blocks: 144,
            mode: FeeEstimateMode::Economical,
        },
        FeeTarget::Lightning(ConfirmationTarget::OutputSpendingFee) => FeeSource::Blocks {
            blocks: 12,
            mode: FeeEstimateMode::Economical,
        },
    }
}

pub fn all_targets() -> [FeeTarget; 10] {
    [
        FeeTarget::OnchainPayment,
        FeeTarget::ChannelFunding,
        ConfirmationTarget::MaximumFeeEstimate.into(),
        ConfirmationTarget::UrgentOnChainSweep.into(),
        ConfirmationTarget::MinAllowedAnchorChannelRemoteFee.into(),
        ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee.into(),
        ConfirmationTarget::AnchorChannelFee.into(),
        ConfirmationTarget::NonAnchorChannelFee.into(),
        ConfirmationTarget::ChannelCloseMinimum.into(),
        ConfirmationTarget::OutputSpendingFee.into(),
    ]
}

/// ldk-node `apply_post_estimation_adjustments`.
///
/// `MinAllowedNonAnchorChannelRemoteFee` is nudged down by 1 sat/vB so a
/// funder whose estimator rounded up is still accepted.
/// `MaximumFeeEstimate` is bumped (`* 11/10 + 2500` sat/kW) so the LDK
/// fee-inflation ceiling is not so tight that a peer a bit above the
/// one-block conservative estimate gets force-closed.
pub fn apply_post_estimation_adjustments(target: FeeTarget, sat_kw: u32) -> u32 {
    match target {
        FeeTarget::Lightning(ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee) => {
            sat_kw.saturating_sub(250).max(FEERATE_FLOOR_SATS_PER_KW)
        }
        FeeTarget::Lightning(ConfirmationTarget::MaximumFeeEstimate) => sat_kw
            .saturating_mul(11)
            .saturating_div(10)
            .saturating_add(2500),
        _ => sat_kw,
    }
}

pub struct FeeCache {
    rates: RwLock<HashMap<FeeTarget, u32>>,
}

impl FeeCache {
    pub fn new() -> Self {
        Self {
            rates: RwLock::new(HashMap::new()),
        }
    }

    pub fn get(&self, target: FeeTarget) -> u32 {
        let rates = self.rates.read().unwrap_or_else(|e| e.into_inner());
        let fallback = fallback_sat_per_kw(target);
        rates
            .get(&target)
            .copied()
            .unwrap_or(fallback)
            .max(FEERATE_FLOOR_SATS_PER_KW)
    }

    /// Replace the cache. Returns whether the map actually changed.
    pub fn set(&self, rates: HashMap<FeeTarget, u32>) -> bool {
        let mut locked = self.rates.write().unwrap_or_else(|e| e.into_inner());
        if *locked != rates {
            *locked = rates;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sat_per_vb_to_kw_is_times_250() {
        assert_eq!(sat_per_vb_to_kw(1), 250);
        assert_eq!(sat_per_vb_to_kw(12), 3000);
    }

    #[test]
    fn channel_funding_is_12_economical_with_1000_fallback() {
        assert_eq!(fallback_sat_per_kw(FeeTarget::ChannelFunding), 1000);
        let FeeSource::Blocks { blocks, mode } = source_for_target(FeeTarget::ChannelFunding)
        else {
            panic!("channel funding must be a block target");
        };
        assert_eq!(blocks, 12);
        assert_eq!(mode, FeeEstimateMode::Economical);
    }

    #[test]
    fn onchain_payment_is_6_economical() {
        let FeeSource::Blocks { blocks, mode } = source_for_target(FeeTarget::OnchainPayment)
        else {
            panic!("onchain payment must be a block target");
        };
        assert_eq!(blocks, 6);
        assert_eq!(mode, FeeEstimateMode::Economical);
    }

    #[test]
    fn cold_cache_accepts_a_253_funder() {
        let cache = FeeCache::new();
        assert_eq!(
            cache.get(ConfirmationTarget::MinAllowedAnchorChannelRemoteFee.into()),
            FEERATE_FLOOR_SATS_PER_KW
        );
        assert_eq!(
            cache.get(ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee.into()),
            FEERATE_FLOOR_SATS_PER_KW
        );
    }

    #[test]
    fn cold_cache_channel_funding_uses_ldk_node_fallback() {
        let cache = FeeCache::new();
        assert_eq!(cache.get(FeeTarget::ChannelFunding), 1000);
    }

    #[test]
    fn non_anchor_min_allowed_subtracts_one_sat_per_vb() {
        let target = ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee.into();
        assert_eq!(
            apply_post_estimation_adjustments(target, sat_per_vb_to_kw(2)),
            FEERATE_FLOOR_SATS_PER_KW
        );
        assert_eq!(
            apply_post_estimation_adjustments(target, sat_per_vb_to_kw(1)),
            FEERATE_FLOOR_SATS_PER_KW
        );
    }

    #[test]
    fn maximum_fee_estimate_gets_ldk_node_leeway() {
        // 8 sat/vB = 2000 sat/kW → 2000 * 11/10 + 2500 = 4700
        assert_eq!(
            apply_post_estimation_adjustments(
                ConfirmationTarget::MaximumFeeEstimate.into(),
                sat_per_vb_to_kw(8),
            ),
            4700
        );
    }

    #[test]
    fn other_targets_are_not_adjusted() {
        assert_eq!(
            apply_post_estimation_adjustments(
                ConfirmationTarget::UrgentOnChainSweep.into(),
                sat_per_vb_to_kw(12),
            ),
            3000
        );
        assert_eq!(
            apply_post_estimation_adjustments(FeeTarget::ChannelFunding, sat_per_vb_to_kw(4)),
            1000
        );
    }

    #[test]
    fn min_allowed_anchor_reads_mempool_min() {
        assert!(matches!(
            source_for_target(ConfirmationTarget::MinAllowedAnchorChannelRemoteFee.into()),
            FeeSource::MempoolMin
        ));
        let FeeSource::Blocks { blocks, mode } =
            source_for_target(ConfirmationTarget::UrgentOnChainSweep.into())
        else {
            panic!("urgent must be a block target");
        };
        assert_eq!(blocks, 6);
        assert_eq!(mode, FeeEstimateMode::Conservative);
    }
}
