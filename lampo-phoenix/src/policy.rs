//! Liquidity policy for on-the-fly funding, mirroring phoenixd's
//! `--auto-liquidity`, `--max-fee-credit`, `--max-relative-fee-percent` and
//! `--max-mining-fee`. It only decides: nothing here opens or splices.

use std::fmt;

use super::conf::PhoenixConf;
use super::liquidity_ads::Fees;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiquidityPolicy {
    /// Liquidity to request when a payment does not fit, in sat. `None`
    /// disables on-the-fly funding.
    pub auto_liquidity_sat: Option<u64>,
    /// Fee credit the LSP may hold for this node, in sat.
    pub max_fee_credit_sat: u64,
    /// Total fee cap relative to the amount received, in basis points.
    pub max_relative_fee_bps: u16,
    /// Mining fee cap per funding transaction, in sat. `None` rejects.
    pub max_mining_fee_sat: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// `phoenix-auto-liquidity` is unset.
    Disabled,
    /// `phoenix-max-mining-fee` is unset, so no mining fee is acceptable.
    MissingMiningFeeCap,
    OverRelativeFee {
        fee_msat: u64,
        max_msat: u64,
        bps: u16,
    },
    OverMiningFee {
        mining_fee_sat: u64,
        max_sat: u64,
    },
    /// The amount cannot pay its own fee even with the fee credit the LSP
    /// holds, and keeping it as credit would exceed the allowed maximum.
    OverFeeCredit {
        credit_msat: u64,
        max_msat: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Buy liquidity and pay the fee from the payment.
    Accept,
    /// The payment is too small to pay the fee: let the LSP keep it as fee
    /// credit (bLIP 41) until enough has accumulated. `credit_msat` is the
    /// credit after this payment.
    AddToFeeCredit {
        credit_msat: u64,
    },
    Reject(RejectReason),
}

impl LiquidityPolicy {
    pub fn from_conf(conf: &PhoenixConf) -> Self {
        Self {
            auto_liquidity_sat: conf.auto_liquidity_sat,
            max_fee_credit_sat: conf.max_fee_credit_sat,
            max_relative_fee_bps: conf.max_relative_fee_bps,
            max_mining_fee_sat: conf.max_mining_fee_sat,
        }
    }

    /// Decide whether paying `fees` to receive `amount_msat`, with
    /// `fee_credit_msat` already held by the LSP, is acceptable.
    ///
    /// Mirrors lightning-kmp: a payment that cannot pay the fee even with
    /// the existing credit becomes fee credit when that stays within
    /// `max_fee_credit_sat`; otherwise the relative fee cap and then the
    /// absolute cap on the mining fee alone decide.
    pub fn evaluate(&self, amount_msat: u64, fee_credit_msat: u64, fees: &Fees) -> PolicyDecision {
        if self.auto_liquidity_sat.is_none() {
            return PolicyDecision::Reject(RejectReason::Disabled);
        }
        let Some(max_mining_fee_sat) = self.max_mining_fee_sat else {
            return PolicyDecision::Reject(RejectReason::MissingMiningFeeCap);
        };
        let fee_msat = fees.total_msat();
        let available_msat = amount_msat.saturating_add(fee_credit_msat);
        if available_msat < fee_msat {
            let max_msat = self.max_fee_credit_sat.saturating_mul(1000);
            return if available_msat <= max_msat {
                PolicyDecision::AddToFeeCredit {
                    credit_msat: available_msat,
                }
            } else {
                PolicyDecision::Reject(RejectReason::OverFeeCredit {
                    credit_msat: available_msat,
                    max_msat,
                })
            };
        }
        let max_relative_msat =
            (u128::from(amount_msat) * u128::from(self.max_relative_fee_bps) / 10_000) as u64;
        if fee_msat > max_relative_msat {
            return PolicyDecision::Reject(RejectReason::OverRelativeFee {
                fee_msat,
                max_msat: max_relative_msat,
                bps: self.max_relative_fee_bps,
            });
        }
        if fees.mining_fee_sat > max_mining_fee_sat {
            return PolicyDecision::Reject(RejectReason::OverMiningFee {
                mining_fee_sat: fees.mining_fee_sat,
                max_sat: max_mining_fee_sat,
            });
        }
        PolicyDecision::Accept
    }
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => write!(f, "phoenix-auto-liquidity is not set"),
            Self::MissingMiningFeeCap => write!(f, "phoenix-max-mining-fee is not set"),
            Self::OverRelativeFee {
                fee_msat,
                max_msat,
                bps,
            } => write!(
                f,
                "fee {fee_msat} msat exceeds {max_msat} msat ({bps} bps of the amount)"
            ),
            Self::OverMiningFee {
                mining_fee_sat,
                max_sat,
            } => write!(
                f,
                "mining fee {mining_fee_sat} sat exceeds phoenix-max-mining-fee {max_sat} sat"
            ),
            Self::OverFeeCredit {
                credit_msat,
                max_msat,
            } => write!(
                f,
                "payment is smaller than its fee and {credit_msat} msat of fee credit exceeds phoenix-max-fee-credit {max_msat} msat"
            ),
        }
    }
}

impl fmt::Display for PolicyDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Accept => write!(f, "accept"),
            Self::AddToFeeCredit { credit_msat } => {
                write!(
                    f,
                    "accept as fee credit ({credit_msat} msat after this payment)"
                )
            }
            Self::Reject(reason) => write!(f, "reject: {reason}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> LiquidityPolicy {
        LiquidityPolicy {
            auto_liquidity_sat: Some(2_000_000),
            max_fee_credit_sat: 0,
            max_relative_fee_bps: 250,
            max_mining_fee_sat: Some(5_000),
        }
    }

    fn fees(mining_fee_sat: u64, service_fee_sat: u64) -> Fees {
        Fees {
            mining_fee_sat,
            service_fee_sat,
        }
    }

    #[test]
    fn disabled_rejects_everything() {
        let mut disabled = policy();
        disabled.auto_liquidity_sat = None;
        assert_eq!(
            disabled.evaluate(1_000_000_000, 0, &fees(0, 0)),
            PolicyDecision::Reject(RejectReason::Disabled)
        );
    }

    #[test]
    fn missing_mining_cap_rejects() {
        let mut capless = policy();
        capless.max_mining_fee_sat = None;
        assert_eq!(
            capless.evaluate(1_000_000_000, 0, &fees(1, 1)),
            PolicyDecision::Reject(RejectReason::MissingMiningFeeCap)
        );
    }

    #[test]
    fn accepts_within_every_cap() {
        // 1M sat received, 2.5% = 25_000 sat allowed; fee 3_000 + 4_000.
        assert_eq!(
            policy().evaluate(1_000_000_000, 0, &fees(3_000, 4_000)),
            PolicyDecision::Accept
        );
    }

    #[test]
    fn rejects_over_relative_fee() {
        // 100k sat received: 2.5% = 2_500 sat; fee 3_000 sat total.
        match policy().evaluate(100_000_000, 0, &fees(1_000, 2_000)) {
            PolicyDecision::Reject(RejectReason::OverRelativeFee {
                fee_msat,
                max_msat,
                bps,
            }) => {
                assert_eq!(fee_msat, 3_000_000);
                assert_eq!(max_msat, 2_500_000);
                assert_eq!(bps, 250);
            }
            other => panic!("expected OverRelativeFee, got {other:?}"),
        }
    }

    #[test]
    fn rejects_over_mining_fee_even_when_relatively_cheap() {
        assert_eq!(
            policy().evaluate(1_000_000_000, 0, &fees(6_000, 0)),
            PolicyDecision::Reject(RejectReason::OverMiningFee {
                mining_fee_sat: 6_000,
                max_sat: 5_000
            })
        );
    }

    #[test]
    fn payment_smaller_than_its_fee_becomes_credit_or_is_rejected() {
        // A 1 sat payment cannot pay a 2 sat fee.
        let fee = fees(1, 1);
        assert_eq!(
            policy().evaluate(1_000, 0, &fee),
            PolicyDecision::Reject(RejectReason::OverFeeCredit {
                credit_msat: 1_000,
                max_msat: 0
            })
        );
        let mut credit = policy();
        credit.max_fee_credit_sat = 1;
        assert_eq!(
            credit.evaluate(1_000, 0, &fee),
            PolicyDecision::AddToFeeCredit { credit_msat: 1_000 }
        );
        // Existing credit counts: 1 sat held plus 1 sat received covers
        // the 2 sat fee, so the ordinary caps apply (and the relative one
        // rejects a fee twice the amount).
        assert!(matches!(
            credit.evaluate(1_000, 1_000, &fee),
            PolicyDecision::Reject(RejectReason::OverRelativeFee { .. })
        ));
        assert_eq!(
            PolicyDecision::Reject(RejectReason::Disabled).to_string(),
            "reject: phoenix-auto-liquidity is not set"
        );
    }
}
