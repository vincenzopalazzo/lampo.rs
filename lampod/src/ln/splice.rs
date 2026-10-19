//! Operator splice-in, splice-out, and fee bump.
//!
//! The calls match ldk-node `Node::splice_in`, `splice_out`, and
//! `bump_channel_funding_fee`: `splice_channel`, a `FundingTemplate`
//! contribution, then `funding_contributed`. Signing happens when LDK emits
//! `FundingTransactionReadyForSigning`.

use std::sync::Arc;
use std::time::Duration;

use lampo_common::bitcoin::address::NetworkUnchecked;
use lampo_common::bitcoin::{Address, Amount, FeeRate, TxOut};
use lampo_common::error;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::ldk::chain::ClaimId;
use lampo_common::ldk::ln::channelmanager::SpliceContributionError;
use lampo_common::ldk::ln::funding::FundingTemplate;
use lampo_common::ldk::util::wallet_utils::CoinSelectionSource;
use lampo_common::model::request::{self, parse_channel_id};
use lampo_common::model::response;
use lampo_common::types::{ChannelId, NodeId};
use lampo_common::wallet::WalletManager;

use super::channel_manager::{LampoChannelManager, FUNDING_WAIT_TIMEOUT_SECS};
use crate::chain::FeeTarget;

/// On-chain sats kept back per anchor channel so a splice-in cannot spend the
/// coins a force-close needs for CPFP. Same default as ldk-node.
const ANCHOR_CHANNEL_RESERVE_SATS: u64 = 25_000;

/// 1.5× the funding feerate, the ceiling ldk-node uses for a splice and its
/// RBF. The max is never raised to meet an RBF minimum.
pub(crate) fn splice_fee_ceiling(estimate: FeeRate) -> FeeRate {
    FeeRate::from_sat_per_kwu(estimate.to_sat_per_kwu().saturating_mul(3) / 2)
}

/// Target and max feerates for replacing a pending splice. `None` when the
/// RBF minimum is above the ceiling.
pub(crate) fn rbf_splice_feerates(
    estimate: FeeRate,
    min_rbf: FeeRate,
) -> Option<(FeeRate, FeeRate)> {
    let max = splice_fee_ceiling(estimate);
    let target = estimate.max(min_rbf);
    (target <= max).then_some((target, max))
}

/// Wallet passed to `FundingTemplate::splice_in`. Coin selection and signing
/// of owned inputs live on [`WalletManager`].
struct SpliceWallet(Arc<dyn WalletManager>);

impl CoinSelectionSource for SpliceWallet {
    fn select_confirmed_utxos<'a>(
        &'a self,
        _claim_id: Option<ClaimId>,
        must_spend: Vec<lampo_common::ldk::util::wallet_utils::Input>,
        must_pay_to: &'a [TxOut],
        target_feerate_sat_per_1000_weight: u32,
        _max_tx_weight: u64,
    ) -> impl std::future::Future<
        Output = Result<lampo_common::ldk::util::wallet_utils::CoinSelection, ()>,
    > + Send
           + 'a {
        let wallet = Arc::clone(&self.0);
        let outputs = must_pay_to.to_vec();
        async move {
            let rate = FeeRate::from_sat_per_kwu(target_feerate_sat_per_1000_weight as u64);
            wallet
                .select_confirmed_utxos(must_spend, outputs, rate)
                .await
                .map_err(|err| {
                    log::error!(target: "lampo", "splice coin selection failed: {err}");
                })
        }
    }

    fn sign_psbt<'a>(
        &'a self,
        psbt: lampo_common::bitcoin::psbt::Psbt,
    ) -> impl std::future::Future<Output = Result<lampo_common::bitcoin::Transaction, ()>> + Send + 'a
    {
        let wallet = Arc::clone(&self.0);
        async move {
            wallet.sign_psbt(psbt).map_err(|err| {
                log::error!(target: "lampo", "splice psbt signing failed: {err}");
            })
        }
    }
}

impl LampoChannelManager {
    pub async fn splice_in(&self, request: request::SpliceIn) -> error::Result<response::Splice> {
        if request.amount == 0 {
            error::bail!("splice amount must be greater than zero");
        }
        let (node_id, channel_id) =
            self.splice_channel_ids(&request.node_id, request.channel_id.as_deref())?;
        let balance = self.wallet_manager().get_onchain_balance().await?;
        let reserve = self.anchor_reserve_sats();
        let available = balance.saturating_sub(reserve);
        if available < request.amount {
            error::bail!(
                "not enough confirmed on-chain funds to splice in {} sats (have {balance}, {reserve} kept for anchor channels)",
                request.amount
            );
        }
        let (min_feerate, max_feerate) = self.splice_feerates(request.sat_per_vbyte)?;
        let mut events = self.handler().events();
        let template = self.begin_splice(&channel_id, &node_id)?;
        if template.prior_contribution().is_some() {
            error::bail!(
                "a splice is already pending on this channel; use bumpchannelfee to raise its fee"
            );
        }
        let feerate = match template.min_rbf_feerate() {
            Some(min_rbf) if min_rbf <= max_feerate => min_feerate.max(min_rbf),
            _ => min_feerate,
        };
        let contribution = template
            .splice_in(
                Amount::from_sat(request.amount),
                feerate,
                max_feerate,
                SpliceWallet(self.wallet_manager()),
            )
            .await
            .map_err(|err| error::anyhow!("failed to build splice-in: {err}"))?;
        self.contribute(&channel_id, &node_id, contribution).await?;
        self.wait_splice(channel_id, node_id, &mut events).await
    }

    pub async fn splice_out(&self, request: request::SpliceOut) -> error::Result<response::Splice> {
        if request.amount == 0 {
            error::bail!("splice amount must be greater than zero");
        }
        let (node_id, channel_id) =
            self.splice_channel_ids(&request.node_id, request.channel_id.as_deref())?;
        let details = self
            .manager()
            .list_channels_with_counterparty(&node_id)
            .into_iter()
            .find(|channel| channel.channel_id == channel_id)
            .ok_or_else(|| error::anyhow!("channel `{channel_id}` not found"))?;
        if request.amount > details.next_splice_out_maximum_sat {
            error::bail!(
                "splice-out of {} sats exceeds the {} sats this channel can send on-chain",
                request.amount,
                details.next_splice_out_maximum_sat
            );
        }
        let address: Address<NetworkUnchecked> = request
            .address
            .parse()
            .map_err(|err| error::anyhow!("invalid address: {err}"))?;
        let address = address
            .require_network(self.conf.network)
            .map_err(|_| error::anyhow!("address is not valid for {}", self.conf.network))?;
        let (min_feerate, max_feerate) = self.splice_feerates(request.sat_per_vbyte)?;
        let mut events = self.handler().events();
        let template = self.begin_splice(&channel_id, &node_id)?;
        if template.prior_contribution().is_some() {
            error::bail!(
                "a splice is already pending on this channel; use bumpchannelfee to raise its fee"
            );
        }
        let feerate = match template.min_rbf_feerate() {
            Some(min_rbf) if min_rbf <= max_feerate => min_feerate.max(min_rbf),
            _ => min_feerate,
        };
        let outputs = vec![TxOut {
            value: Amount::from_sat(request.amount),
            script_pubkey: address.script_pubkey(),
        }];
        let contribution = template
            .splice_out(outputs, feerate, max_feerate)
            .map_err(|err| error::anyhow!("failed to build splice-out: {err}"))?;
        self.contribute(&channel_id, &node_id, contribution).await?;
        self.wait_splice(channel_id, node_id, &mut events).await
    }

    pub async fn bump_channel_fee(
        &self,
        request: request::BumpChannelFee,
    ) -> error::Result<response::Splice> {
        let (node_id, channel_id) =
            self.splice_channel_ids(&request.node_id, request.channel_id.as_deref())?;
        let (min_feerate, _) = self.splice_feerates(request.sat_per_vbyte)?;
        let mut events = self.handler().events();
        let template = self.begin_splice(&channel_id, &node_id)?;
        let Some(min_rbf) = template.min_rbf_feerate() else {
            error::bail!("no pending splice to replace on channel `{channel_id}`");
        };
        let Some((target, max_feerate)) = rbf_splice_feerates(min_feerate, min_rbf) else {
            error::bail!(
                "replacing the pending splice needs {} sat/kW, above the {} sat/kW ceiling",
                min_rbf.to_sat_per_kwu(),
                splice_fee_ceiling(min_feerate).to_sat_per_kwu()
            );
        };
        let contribution = template
            .rbf_prior_contribution(
                Some(target),
                max_feerate,
                SpliceWallet(self.wallet_manager()),
            )
            .await
            .map_err(|err| error::anyhow!("failed to bump splice fee: {err}"))?;
        self.contribute(&channel_id, &node_id, contribution).await?;
        self.wait_splice(channel_id, node_id, &mut events).await
    }

    fn splice_channel_ids(
        &self,
        node_id: &str,
        channel_id: Option<&str>,
    ) -> error::Result<(NodeId, ChannelId)> {
        let node_id = node_id
            .parse::<NodeId>()
            .map_err(|err| error::anyhow!("invalid `node_id`: {err}"))?;
        let channels = self
            .manager()
            .list_channels_with_counterparty(&node_id)
            .into_iter()
            .filter(|channel| channel.is_channel_ready)
            .collect::<Vec<_>>();
        let channel_id = match channel_id {
            Some(id) => {
                let id = parse_channel_id(id)?;
                if !channels.iter().any(|channel| channel.channel_id == id) {
                    error::bail!("no ready channel `{id}` with peer `{node_id}`");
                }
                id
            }
            None if channels.len() == 1 => channels[0].channel_id,
            None if channels.is_empty() => {
                error::bail!("no ready channel with peer `{node_id}`")
            }
            None => error::bail!(
                "peer `{node_id}` has {} ready channels; pass `channel_id`",
                channels.len()
            ),
        };
        Ok((node_id, channel_id))
    }

    fn anchor_reserve_sats(&self) -> u64 {
        let anchors = self
            .manager()
            .list_channels()
            .into_iter()
            .filter(|channel| {
                channel.channel_type.as_ref().map_or(true, |features| {
                    features.supports_anchors_zero_fee_htlc_tx()
                        || features.supports_anchor_zero_fee_commitments()
                })
            })
            .count() as u64;
        anchors.saturating_mul(ANCHOR_CHANNEL_RESERVE_SATS)
    }

    fn splice_feerates(&self, sat_per_vbyte: Option<u64>) -> error::Result<(FeeRate, FeeRate)> {
        let min = match sat_per_vbyte {
            Some(sat_per_vbyte) => FeeRate::from_sat_per_kwu(sat_per_vbyte.saturating_mul(250)),
            None => self.onchain.estimate_fee_rate(FeeTarget::ChannelFunding),
        };
        Ok((min, splice_fee_ceiling(min)))
    }

    fn begin_splice(
        &self,
        channel_id: &ChannelId,
        node_id: &NodeId,
    ) -> error::Result<FundingTemplate> {
        self.manager()
            .splice_channel(channel_id, node_id)
            .map_err(|err| error::anyhow!("{err:?}"))
    }

    async fn contribute(
        &self,
        channel_id: &ChannelId,
        node_id: &NodeId,
        contribution: lampo_common::ldk::ln::funding::FundingContribution,
    ) -> error::Result<()> {
        let locktime = self.splice_locktime().await;
        self.manager()
            .funding_contributed(channel_id, node_id, contribution, locktime)
            .map_err(|err| match err {
                SpliceContributionError::ContributionPending => error::anyhow!(
                    "a splice is already pending on this channel; use bumpchannelfee to raise its fee"
                ),
                other => error::anyhow!("{other}"),
            })
    }

    /// Same clamp as channel open (issue #572): a locktime ahead of the
    /// backend tip makes bitcoind reject the splice as non-final.
    async fn splice_locktime(&self) -> Option<u32> {
        let ldk_height = self.manager().current_best_block().height;
        let chain_height = self
            .onchain
            .backend
            .get_best_block()
            .await
            .ok()
            .and_then(|(_, height)| height)
            .unwrap_or(ldk_height);
        Some(ldk_height.min(chain_height).saturating_sub(1))
    }

    async fn wait_splice(
        &self,
        channel_id: ChannelId,
        node_id: NodeId,
        events: &mut lampo_common::chan::UnboundedReceiver<Event>,
    ) -> error::Result<response::Splice> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(FUNDING_WAIT_TIMEOUT_SECS);
        loop {
            if tokio::time::Instant::now() >= deadline {
                // A splice that never finishes `stfu` leaves the channel
                // quiescent. Cancel while our signatures are still unreleased.
                // If they already went out, cancel fails and the round may
                // still confirm.
                match self
                    .manager()
                    .cancel_funding_contributed(&channel_id, &node_id)
                {
                    Ok(()) => error::bail!(
                        "splice negotiation timed out after {FUNDING_WAIT_TIMEOUT_SECS}s with peer {node_id}; the splice was canceled"
                    ),
                    Err(err) => error::bail!(
                        "splice negotiation timed out after {FUNDING_WAIT_TIMEOUT_SECS}s with peer {node_id}; cancel failed ({err:?}) and the splice may still confirm"
                    ),
                }
            }
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Some(Event::Lightning(LightningEvent::SpliceNegotiated {
                    channel_id: id,
                    funding_txo,
                    ..
                }))) if id == channel_id => {
                    return Ok(response::Splice {
                        node_id: node_id.to_string(),
                        channel_id: channel_id.to_string(),
                        txid: funding_txo.txid.to_string(),
                        vout: funding_txo.vout,
                    });
                }
                Ok(Some(Event::Lightning(LightningEvent::SpliceFailed {
                    channel_id: id,
                    reason,
                    ..
                }))) if id == channel_id => {
                    error::bail!("{reason}");
                }
                Ok(Some(_)) => {}
                Ok(None) => error::bail!("splice negotiation: event bus closed"),
                Err(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use lampo_common::bitcoin::FeeRate;

    use super::{rbf_splice_feerates, splice_fee_ceiling};

    #[test]
    fn splice_ceiling_is_one_and_a_half_times_the_estimate() {
        let estimate = FeeRate::from_sat_per_kwu(1000);
        assert_eq!(splice_fee_ceiling(estimate).to_sat_per_kwu(), 1500);
    }

    #[test]
    fn rbf_pays_the_minimum_without_raising_the_ceiling() {
        let estimate = FeeRate::from_sat_per_kwu(253);
        let min_rbf = FeeRate::from_sat_per_kwu(278);
        let (target, max) = rbf_splice_feerates(estimate, min_rbf).unwrap();
        assert_eq!(target.to_sat_per_kwu(), 278);
        assert_eq!(max.to_sat_per_kwu(), 253 * 3 / 2);
    }

    #[test]
    fn rbf_refuses_a_minimum_above_the_ceiling() {
        let estimate = FeeRate::from_sat_per_kwu(100);
        let min_rbf = FeeRate::from_sat_per_kwu(1000);
        assert!(rbf_splice_feerates(estimate, min_rbf).is_none());
    }
}
