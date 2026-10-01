//! The custom message handler installed in the peer manager: it advertises
//! the Phoenix feature bits to the configured LSP only, parses every
//! Phoenix message, keeps what the LSP told us, and decides (without acting)
//! about on-the-fly funding proposals.
//!
//! LDK calls into this handler while holding its own peer locks, so nothing
//! here calls back into the peer manager; messages to send are queued and
//! drained through `get_and_clear_pending_msg`, and the caller that queued
//! them flushes the peer manager afterwards.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use lampo_common::bitcoin::constants::ChainHash;
use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::bitcoin::Network;
use lampo_common::conf::PhoenixLspPeer;
use lampo_common::error;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::hex;
use lampo_common::ldk::ln::msgs::{DecodeError, Init, LightningError};
use lampo_common::ldk::ln::peer_handler::CustomMessageHandler;
use lampo_common::ldk::ln::wire::CustomMessageReader;
use lampo_common::ldk::types::features::{InitFeatures, NodeFeatures};
use lampo_common::ldk::types::payment::{PaymentHash, PaymentPreimage};
use lampo_common::ldk::util::ser::LengthLimitedRead;

use super::liquidity_ads::WillFundRates;
use super::policy::{LiquidityPolicy, PolicyDecision};
use super::purchases::{unix_now, PurchaseStore};
use super::wire::{
    self, AddFeeCredit, DnsAddressRequest, PhoenixLspMessage, RecommendedFeerates, WillAddHtlc,
};

/// Optional `on_the_fly_funding`, advertised to the LSP only.
pub const ON_THE_FLY_FUNDING_BIT: usize = 561;
/// Optional `funding_fee_credit`, advertised to the LSP only.
pub const FUNDING_FEE_CREDIT_BIT: usize = 563;
/// Optional `zero_reserve_channels`, advertised to the LSP only.
pub const ZERO_RESERVE_CHANNELS_BIT: usize = 129;

const LOG_TARGET: &str = "phoenix-lsp";
/// Pending proposals and remembered invoices are bounded so a chatty LSP
/// cannot grow memory without limit.
const MAX_PENDING_PROPOSALS: usize = 1_000;
const MAX_REMEMBERED_INVOICES: usize = 10_000;

/// Features with exactly `bits` set, in BOLT 9 little-endian byte order.
pub fn init_features_with_bits(bits: &[usize]) -> InitFeatures {
    let len = bits.iter().max().map_or(0, |max| max / 8 + 1);
    let mut flags = vec![0u8; len];
    for bit in bits {
        flags[bit / 8] |= 1 << (bit % 8);
    }
    InitFeatures::from_le_bytes(flags)
}

/// Whether `bit` is set in `features`.
pub fn has_feature_bit(features: &InitFeatures, bit: usize) -> bool {
    features
        .le_flags()
        .get(bit / 8)
        .is_some_and(|byte| byte & (1 << (bit % 8)) != 0)
}

/// Whether the required or the optional bit of a feature pair is set.
fn supports_feature(features: &InitFeatures, required_bit: usize) -> bool {
    has_feature_bit(features, required_bit) || has_feature_bit(features, required_bit + 1)
}

/// Every bit set in `features`, lowest first.
pub fn feature_bits(features: &InitFeatures) -> Vec<u16> {
    let mut bits = Vec::new();
    for (index, byte) in features.le_flags().iter().enumerate() {
        for bit in 0..8 {
            if byte & (1 << bit) != 0 {
                bits.push((index * 8 + bit) as u16);
            }
        }
    }
    bits
}

/// What the liquidity policy made of a `will_add_htlc`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WillAddHtlcDecision {
    /// The payment hash is not one of an unpaid invoice this node issued.
    UnknownPaymentHash,
    /// The LSP has not sent `recommended_feerates` yet.
    MissingFeerates,
    /// No `will_fund_rates` were recorded for the LSP.
    MissingFundingRates,
    /// The LSP's rate card does not cover the configured amount.
    NoRateForAmount {
        amount_sat: u64,
    },
    Policy(PolicyDecision),
}

impl fmt::Display for WillAddHtlcDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPaymentHash => {
                write!(
                    f,
                    "undecided: payment hash is not an unpaid invoice of ours"
                )
            }
            Self::MissingFeerates => write!(f, "undecided: no recommended_feerates from the LSP"),
            Self::MissingFundingRates => write!(f, "undecided: no will_fund_rates recorded"),
            Self::NoRateForAmount { amount_sat } => {
                write!(f, "undecided: no funding rate covers {amount_sat} sat")
            }
            Self::Policy(decision) => write!(f, "{decision}"),
        }
    }
}

/// A `will_add_htlc` the LSP sent and this node has not acted on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingWillAddHtlc {
    pub msg: WillAddHtlc,
    /// Unix seconds.
    pub received_at: u64,
    pub decision: WillAddHtlcDecision,
}

/// A copy of what the handler knows, for `phoenixlsp-info`.
#[derive(Clone, Debug)]
pub struct PhoenixLspSnapshot {
    pub lsp: Option<PhoenixLspPeer>,
    pub connected: bool,
    pub lsp_init_features: Option<InitFeatures>,
    pub feerates: Option<RecommendedFeerates>,
    pub fee_credit_msat: u64,
    pub pending: Vec<PendingWillAddHtlc>,
}

struct State {
    lsp: Option<PhoenixLspPeer>,
    connected: bool,
    lsp_init_features: Option<InitFeatures>,
    feerates: Option<RecommendedFeerates>,
    fee_credit_msat: u64,
    will_fund_rates: Option<WillFundRates>,
    pending: BTreeMap<[u8; 32], PendingWillAddHtlc>,
    /// Payment hashes of invoices this node issued and has not claimed,
    /// with their amount when the invoice had one.
    issued_invoices: HashMap<PaymentHash, Option<u64>>,
    outbound: Vec<(PublicKey, PhoenixLspMessage)>,
}

pub struct PhoenixLspHandler {
    chain_hash: ChainHash,
    lsp_features: InitFeatures,
    policy: LiquidityPolicy,
    purchases: Arc<PurchaseStore>,
    /// Whether a channel with the LSP exists; a purchase without one also
    /// pays the channel creation fee.
    has_lsp_channel: AtomicBool,
    /// The event bus, bound once the daemon has built it.
    handler: OnceLock<Arc<dyn Handler>>,
    state: Mutex<State>,
}

impl PhoenixLspHandler {
    pub fn new(
        lsp: Option<PhoenixLspPeer>,
        network: Network,
        policy: LiquidityPolicy,
        purchases: Arc<PurchaseStore>,
    ) -> Self {
        match &lsp {
            Some(lsp) => log::info!(target: LOG_TARGET, "Phoenix LSP client enabled for `{lsp}`"),
            None => log::debug!(target: LOG_TARGET, "no phoenix-lsp configured; handler idle"),
        }
        Self {
            chain_hash: ChainHash::using_genesis_block_const(network),
            lsp_features: init_features_with_bits(&[
                ZERO_RESERVE_CHANNELS_BIT,
                ON_THE_FLY_FUNDING_BIT,
                FUNDING_FEE_CREDIT_BIT,
            ]),
            policy,
            purchases,
            has_lsp_channel: AtomicBool::new(false),
            handler: OnceLock::new(),
            state: Mutex::new(State {
                lsp,
                connected: false,
                lsp_init_features: None,
                feerates: None,
                fee_credit_msat: 0,
                will_fund_rates: None,
                pending: BTreeMap::new(),
                issued_invoices: HashMap::new(),
                outbound: Vec::new(),
            }),
        }
    }

    /// Bind the event bus. Events emitted before this are dropped.
    pub fn set_handler(&self, handler: Arc<dyn Handler>) {
        if self.handler.set(handler).is_err() {
            log::warn!(target: LOG_TARGET, "event handler already bound");
        }
    }

    pub fn purchases(&self) -> &PurchaseStore {
        &self.purchases
    }

    pub fn policy(&self) -> &LiquidityPolicy {
        &self.policy
    }

    pub fn lsp_peer(&self) -> Option<PhoenixLspPeer> {
        self.lock().lsp.clone()
    }

    pub fn lsp_node_id(&self) -> Option<PublicKey> {
        self.lock().lsp.as_ref().map(|lsp| lsp.node_id)
    }

    pub fn is_lsp(&self, node_id: &PublicKey) -> bool {
        self.lock()
            .lsp
            .as_ref()
            .is_some_and(|lsp| lsp.node_id == *node_id)
    }

    pub fn is_connected(&self) -> bool {
        self.lock().connected
    }

    pub fn snapshot(&self) -> PhoenixLspSnapshot {
        let state = self.lock();
        PhoenixLspSnapshot {
            lsp: state.lsp.clone(),
            connected: state.connected,
            lsp_init_features: state.lsp_init_features.clone(),
            feerates: state.feerates.clone(),
            fee_credit_msat: state.fee_credit_msat,
            pending: state.pending.values().cloned().collect(),
        }
    }

    pub fn set_has_lsp_channel(&self, has_channel: bool) {
        self.has_lsp_channel.store(has_channel, Ordering::Release);
    }

    /// Record the LSP's rate card once something can read it.
    pub fn set_will_fund_rates(&self, rates: WillFundRates) {
        self.lock().will_fund_rates = Some(rates);
    }

    /// Remember an invoice this node issued, so a `will_add_htlc` for it
    /// can be told apart from one for a payment we never asked for.
    pub fn remember_invoice(&self, payment_hash: PaymentHash, amount_msat: Option<u64>) {
        let mut state = self.lock();
        if state.issued_invoices.len() >= MAX_REMEMBERED_INVOICES {
            log::warn!(
                target: LOG_TARGET,
                "forgetting {} remembered invoices: too many unpaid ones",
                state.issued_invoices.len()
            );
            state.issued_invoices.clear();
        }
        state.issued_invoices.insert(payment_hash, amount_msat);
    }

    /// Forget an invoice once it was claimed or cannot be paid any more.
    pub fn forget_invoice(&self, payment_hash: &PaymentHash) {
        self.lock().issued_invoices.remove(payment_hash);
    }

    /// Queue a `dns_address_request` for `offer` (its TLV stream) in
    /// `language`. The caller flushes the peer manager afterwards.
    pub fn send_dns_address_request(&self, offer: Vec<u8>, language: &str) -> error::Result<()> {
        let msg = PhoenixLspMessage::DnsAddressRequest(DnsAddressRequest {
            chain_hash: self.chain_hash,
            offer,
            language: language.to_owned(),
        });
        self.queue_to_lsp(msg)
    }

    /// Queue an `add_fee_credit` revealing `preimage` so the LSP keeps the
    /// payment as fee credit. The caller flushes the peer manager afterwards.
    pub fn send_add_fee_credit(&self, preimage: PaymentPreimage) -> error::Result<()> {
        let msg = PhoenixLspMessage::AddFeeCredit(AddFeeCredit {
            chain_hash: self.chain_hash,
            preimage,
        });
        self.queue_to_lsp(msg)
    }

    /// Test hook: queue any message to any peer, bypassing the LSP check.
    #[cfg(feature = "testing")]
    pub fn send_raw(&self, to: PublicKey, msg: PhoenixLspMessage) {
        self.lock().outbound.push((to, msg));
    }

    /// Test hook: treat `peer` as the LSP from now on.
    #[cfg(feature = "testing")]
    pub fn set_lsp_for_test(&self, peer: PhoenixLspPeer) {
        self.lock().lsp = Some(peer);
    }

    fn queue_to_lsp(&self, msg: PhoenixLspMessage) -> error::Result<()> {
        let mut state = self.lock();
        let Some(lsp) = state.lsp.as_ref() else {
            error::bail!("no phoenix-lsp configured");
        };
        if !state.connected {
            error::bail!("phoenix LSP `{}` is not connected", lsp.node_id);
        }
        let node_id = lsp.node_id;
        log::debug!(target: LOG_TARGET, "queueing {} to the LSP", msg.name());
        state.outbound.push((node_id, msg));
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn emit(&self, event: LightningEvent) {
        match self.handler.get() {
            Some(handler) => handler.emit(Event::Lightning(event)),
            None => log::debug!(target: LOG_TARGET, "no event bus yet, dropping {event:?}"),
        }
    }

    fn chain_matches(&self, chain_hash: ChainHash, what: &str) -> bool {
        if chain_hash == self.chain_hash {
            return true;
        }
        log::warn!(target: LOG_TARGET, "ignoring {what} for another chain ({chain_hash})");
        false
    }

    /// Decide what the policy makes of `msg`. Decision only: nothing is
    /// bought, and the LSP is not answered.
    fn decide(&self, state: &State, msg: &WillAddHtlc) -> WillAddHtlcDecision {
        if !state.issued_invoices.contains_key(&msg.payment_hash) {
            return WillAddHtlcDecision::UnknownPaymentHash;
        }
        let Some(amount_sat) = self.policy.auto_liquidity_sat else {
            return WillAddHtlcDecision::Policy(self.policy.evaluate(
                msg.amount_msat,
                state.fee_credit_msat,
                &Default::default(),
            ));
        };
        let Some(feerates) = state.feerates.as_ref() else {
            return WillAddHtlcDecision::MissingFeerates;
        };
        let Some(rates) = state.will_fund_rates.as_ref() else {
            return WillAddHtlcDecision::MissingFundingRates;
        };
        let Some(rate) = rates.find_rate(amount_sat) else {
            return WillAddHtlcDecision::NoRateForAmount { amount_sat };
        };
        let is_channel_creation = !self.has_lsp_channel.load(Ordering::Acquire);
        let fees = rate.fees(
            feerates.funding_feerate,
            amount_sat,
            amount_sat,
            is_channel_creation,
        );
        WillAddHtlcDecision::Policy(self.policy.evaluate(
            msg.amount_msat,
            state.fee_credit_msat,
            &fees,
        ))
    }

    fn on_will_add_htlc(&self, msg: WillAddHtlc) {
        let mut state = self.lock();
        let decision = self.decide(&state, &msg);
        log::info!(
            target: LOG_TARGET,
            "will_add_htlc `{}` for {} msat (payment hash `{}`, cltv {}): {decision}",
            hex::encode(msg.id),
            msg.amount_msat,
            hex::encode(msg.payment_hash.0),
            msg.cltv_expiry
        );
        if state.pending.len() >= MAX_PENDING_PROPOSALS && !state.pending.contains_key(&msg.id) {
            log::warn!(
                target: LOG_TARGET,
                "dropping {} pending will_add_htlc: too many outstanding proposals",
                state.pending.len()
            );
            state.pending.clear();
        }
        let event = LightningEvent::PhoenixLspWillAddHtlc {
            id: hex::encode(msg.id),
            amount_msat: msg.amount_msat,
            payment_hash: hex::encode(msg.payment_hash.0),
            cltv_expiry: msg.cltv_expiry,
            decision: decision.to_string(),
        };
        state.pending.insert(
            msg.id,
            PendingWillAddHtlc {
                msg,
                received_at: unix_now(),
                decision,
            },
        );
        drop(state);
        self.emit(event);
    }
}

impl CustomMessageReader for PhoenixLspHandler {
    type CustomMessage = PhoenixLspMessage;

    fn read<R: LengthLimitedRead>(
        &self,
        message_type: u16,
        buffer: &mut R,
    ) -> Result<Option<Self::CustomMessage>, DecodeError> {
        wire::read(message_type, buffer)
    }
}

impl CustomMessageHandler for PhoenixLspHandler {
    fn handle_custom_message(
        &self,
        msg: PhoenixLspMessage,
        sender_node_id: PublicKey,
    ) -> Result<(), LightningError> {
        if !self.is_lsp(&sender_node_id) {
            log::warn!(
                target: LOG_TARGET,
                "dropping {} from `{sender_node_id}`: not the configured Phoenix LSP",
                msg.name()
            );
            return Ok(());
        }
        match msg {
            PhoenixLspMessage::RecommendedFeerates(feerates) => {
                if self.chain_matches(feerates.chain_hash, "recommended_feerates") {
                    log::debug!(
                        target: LOG_TARGET,
                        "LSP recommends funding {} sat/kw, commitment {} sat/kw",
                        feerates.funding_feerate,
                        feerates.commitment_feerate
                    );
                    self.lock().feerates = Some(feerates);
                }
            }
            PhoenixLspMessage::CurrentFeeCredit(credit) => {
                if self.chain_matches(credit.chain_hash, "current_fee_credit") {
                    log::info!(target: LOG_TARGET, "LSP holds {} msat of fee credit", credit.amount_msat);
                    self.lock().fee_credit_msat = credit.amount_msat;
                }
            }
            PhoenixLspMessage::WillAddHtlc(proposal) => {
                if self.chain_matches(proposal.chain_hash, "will_add_htlc") {
                    self.on_will_add_htlc(proposal);
                }
            }
            PhoenixLspMessage::CancelOnTheFlyFunding(cancel) => {
                let reason = cancel.reason_str();
                log::warn!(
                    target: LOG_TARGET,
                    "LSP cancelled on-the-fly funding on `{}` for {} payment(s): {reason}",
                    cancel.channel_id,
                    cancel.payment_hashes.len()
                );
                self.lock().pending.retain(|_, pending| {
                    !cancel.payment_hashes.contains(&pending.msg.payment_hash)
                });
                self.emit(LightningEvent::PhoenixLspFundingCancelled {
                    channel_id: cancel.channel_id.to_string(),
                    payment_hashes: cancel
                        .payment_hashes
                        .iter()
                        .map(|hash| hex::encode(hash.0))
                        .collect(),
                    reason,
                });
            }
            PhoenixLspMessage::DnsAddressResponse(response) => {
                if self.chain_matches(response.chain_hash, "dns_address_response") {
                    log::info!(target: LOG_TARGET, "LSP published address `{}`", response.address);
                    self.emit(LightningEvent::PhoenixLspDnsAddress {
                        address: response.address,
                    });
                }
            }
            PhoenixLspMessage::DnsAddressRequest(request) => {
                // Lampo is not an LSP; surface it so a test hook can answer.
                if self.chain_matches(request.chain_hash, "dns_address_request") {
                    log::debug!(target: LOG_TARGET, "dns_address_request from `{sender_node_id}` (not served)");
                    self.emit(LightningEvent::PhoenixLspDnsAddressRequest {
                        counterparty_node_id: sender_node_id,
                        offer: hex::encode(request.offer),
                        language: request.language,
                    });
                }
            }
            PhoenixLspMessage::WillFailHtlc(_)
            | PhoenixLspMessage::WillFailMalformedHtlc(_)
            | PhoenixLspMessage::AddFeeCredit(_) => {
                log::debug!(
                    target: LOG_TARGET,
                    "ignoring {}: only a client sends it",
                    msg.name()
                );
            }
        }
        Ok(())
    }

    fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, PhoenixLspMessage)> {
        std::mem::take(&mut self.lock().outbound)
    }

    fn peer_disconnected(&self, their_node_id: PublicKey) {
        let mut state = self.lock();
        if state
            .lsp
            .as_ref()
            .is_some_and(|lsp| lsp.node_id == their_node_id)
        {
            log::info!(target: LOG_TARGET, "Phoenix LSP `{their_node_id}` disconnected");
            state.connected = false;
        }
    }

    fn peer_connected(
        &self,
        their_node_id: PublicKey,
        msg: &Init,
        _inbound: bool,
    ) -> Result<(), ()> {
        let mut state = self.lock();
        if !state
            .lsp
            .as_ref()
            .is_some_and(|lsp| lsp.node_id == their_node_id)
        {
            return Ok(());
        }
        let on_the_fly_funding = supports_feature(&msg.features, ON_THE_FLY_FUNDING_BIT - 1);
        let funding_fee_credit = supports_feature(&msg.features, FUNDING_FEE_CREDIT_BIT - 1);
        log::info!(
            target: LOG_TARGET,
            "Phoenix LSP `{their_node_id}` connected (on_the_fly_funding={on_the_fly_funding}, funding_fee_credit={funding_fee_credit})"
        );
        state.connected = true;
        state.lsp_init_features = Some(msg.features.clone());
        drop(state);
        self.emit(LightningEvent::PhoenixLspConnected {
            counterparty_node_id: their_node_id,
            on_the_fly_funding,
            funding_fee_credit,
        });
        Ok(())
    }

    fn provided_node_features(&self) -> NodeFeatures {
        NodeFeatures::empty()
    }

    fn provided_init_features(&self, their_node_id: PublicKey) -> InitFeatures {
        if self.is_lsp(&their_node_id) {
            self.lsp_features.clone()
        } else {
            InitFeatures::empty()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use lampo_common::chan::UnboundedReceiver;
    use lampo_common::event::{Emitter, Subscriber};
    use lampo_common::ldk::types::payment::PaymentPreimage;

    use super::super::liquidity_ads::{FundingRate, PaymentType};
    use super::super::policy::RejectReason;
    use super::super::wire::{CancelOnTheFlyFunding, CurrentFeeCredit, DnsAddressResponse};
    use super::*;

    const LSP: &str =
        "03933884aaf1d6b108397e5efe5c86bcf2d8ca8d2f700eda99db9214fc2712b134@127.0.0.1:9735";
    const OTHER: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    struct Bus {
        emitter: Emitter<Event>,
        subscriber: Subscriber<Event>,
    }

    impl Handler for Bus {
        fn events(&self) -> UnboundedReceiver<Event> {
            self.subscriber.subscribe()
        }

        fn emit(&self, event: Event) {
            self.emitter.emit(event)
        }
    }

    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lampo-phoenix-{name}-{}-{}",
            std::process::id(),
            unix_now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn policy() -> LiquidityPolicy {
        LiquidityPolicy {
            auto_liquidity_sat: Some(100_000),
            max_fee_credit_sat: 0,
            max_relative_fee_bps: 250,
            max_mining_fee_sat: Some(5_000),
        }
    }

    fn make_handler(
        lsp: Option<&str>,
        policy: LiquidityPolicy,
    ) -> (PhoenixLspHandler, UnboundedReceiver<Event>) {
        let purchases = Arc::new(PurchaseStore::open(&scratch_dir("handler")).unwrap());
        let handler = PhoenixLspHandler::new(
            lsp.map(|raw| PhoenixLspPeer::from_str(raw).unwrap()),
            Network::Regtest,
            policy,
            purchases,
        );
        let emitter = Emitter::default();
        let bus = Arc::new(Bus {
            subscriber: emitter.subscriber(),
            emitter,
        });
        let events = bus.events();
        handler.set_handler(bus);
        (handler, events)
    }

    fn lsp_id() -> PublicKey {
        PhoenixLspPeer::from_str(LSP).unwrap().node_id
    }

    fn other_id() -> PublicKey {
        PublicKey::from_str(OTHER).unwrap()
    }

    fn regtest() -> ChainHash {
        ChainHash::using_genesis_block_const(Network::Regtest)
    }

    fn connect(handler: &PhoenixLspHandler, node_id: PublicKey, bits: &[usize]) {
        let init = Init {
            features: init_features_with_bits(bits),
            networks: None,
            remote_network_address: None,
        };
        handler.peer_connected(node_id, &init, false).unwrap();
    }

    fn feerates() -> PhoenixLspMessage {
        PhoenixLspMessage::RecommendedFeerates(RecommendedFeerates {
            chain_hash: regtest(),
            funding_feerate: 1_000,
            commitment_feerate: 500,
            funding_feerate_range: None,
            commitment_feerate_range: None,
        })
    }

    fn proposal(payment_hash: PaymentHash, amount_msat: u64) -> WillAddHtlc {
        WillAddHtlc {
            chain_hash: regtest(),
            id: [0x22; 32],
            amount_msat,
            payment_hash,
            cltv_expiry: 400,
            onion_routing_packet: Box::new([0; wire::ONION_PACKET_LEN]),
            path_key: None,
        }
    }

    #[test]
    fn feature_bit_helpers() {
        let features = init_features_with_bits(&[129, 561, 563]);
        assert_eq!(features.le_flags().len(), 71);
        assert_eq!(features.le_flags()[16], 0x02);
        assert_eq!(features.le_flags()[70], 0x0a);
        assert!(has_feature_bit(&features, 561));
        assert!(!has_feature_bit(&features, 560));
        assert!(!has_feature_bit(&features, 1_000));
        assert!(supports_feature(&features, 560));
        assert!(!supports_feature(&features, 562 + 2));
        assert_eq!(feature_bits(&features), vec![129, 561, 563]);
        assert!(init_features_with_bits(&[]).le_flags().is_empty());
    }

    #[test]
    fn advertises_bits_to_the_lsp_only() {
        let (handler, _events) = make_handler(Some(LSP), policy());
        assert_eq!(
            feature_bits(&handler.provided_init_features(lsp_id())),
            vec![129, 561, 563]
        );
        assert!(handler
            .provided_init_features(other_id())
            .le_flags()
            .is_empty());
        assert_eq!(handler.provided_node_features(), NodeFeatures::empty());

        let (idle, _events) = make_handler(None, policy());
        assert!(idle.provided_init_features(lsp_id()).le_flags().is_empty());
        assert!(!idle.is_lsp(&lsp_id()));
    }

    #[test]
    fn records_the_handshake_and_state_from_the_lsp() {
        let (handler, mut events) = make_handler(Some(LSP), policy());
        assert!(!handler.is_connected());
        connect(&handler, other_id(), &[561]);
        assert!(!handler.is_connected());
        assert!(events.try_recv().is_err(), "other peers emit nothing");

        connect(&handler, lsp_id(), &[560, 563]);
        assert!(handler.is_connected());
        match events.try_recv().unwrap() {
            Event::Lightning(LightningEvent::PhoenixLspConnected {
                counterparty_node_id,
                on_the_fly_funding,
                funding_fee_credit,
            }) => {
                assert_eq!(counterparty_node_id, lsp_id());
                assert!(on_the_fly_funding);
                assert!(funding_fee_credit);
            }
            other => panic!("unexpected {other:?}"),
        }

        handler.handle_custom_message(feerates(), lsp_id()).unwrap();
        handler
            .handle_custom_message(
                PhoenixLspMessage::CurrentFeeCredit(CurrentFeeCredit {
                    chain_hash: regtest(),
                    amount_msat: 1_234,
                }),
                lsp_id(),
            )
            .unwrap();
        let snapshot = handler.snapshot();
        assert_eq!(snapshot.feerates.unwrap().funding_feerate, 1_000);
        assert_eq!(snapshot.fee_credit_msat, 1_234);
        assert_eq!(
            feature_bits(&snapshot.lsp_init_features.unwrap()),
            vec![560, 563]
        );

        // Another chain is ignored.
        handler
            .handle_custom_message(
                PhoenixLspMessage::CurrentFeeCredit(CurrentFeeCredit {
                    chain_hash: ChainHash::using_genesis_block_const(Network::Bitcoin),
                    amount_msat: 9,
                }),
                lsp_id(),
            )
            .unwrap();
        assert_eq!(handler.snapshot().fee_credit_msat, 1_234);

        handler.peer_disconnected(lsp_id());
        assert!(!handler.is_connected());
    }

    #[test]
    fn drops_messages_from_other_peers() {
        let (handler, mut events) = make_handler(Some(LSP), policy());
        connect(&handler, lsp_id(), &[561]);
        let _ = events.try_recv();
        handler
            .handle_custom_message(feerates(), other_id())
            .unwrap();
        assert!(handler.snapshot().feerates.is_none());
        handler
            .handle_custom_message(
                PhoenixLspMessage::WillAddHtlc(proposal(PaymentHash([1; 32]), 1)),
                other_id(),
            )
            .unwrap();
        assert!(handler.snapshot().pending.is_empty());
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn will_add_htlc_is_kept_and_decided_without_a_reply() {
        let (handler, mut events) = make_handler(Some(LSP), policy());
        connect(&handler, lsp_id(), &[561]);
        let _ = events.try_recv();
        let hash = PaymentHash([0x33; 32]);

        // Unknown hash: not one of our invoices.
        handler
            .handle_custom_message(
                PhoenixLspMessage::WillAddHtlc(proposal(hash, 1_000)),
                lsp_id(),
            )
            .unwrap();
        let pending = handler.snapshot().pending;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].decision, WillAddHtlcDecision::UnknownPaymentHash);
        match events.try_recv().unwrap() {
            Event::Lightning(LightningEvent::PhoenixLspWillAddHtlc {
                id,
                amount_msat,
                payment_hash,
                cltv_expiry,
                decision,
            }) => {
                assert_eq!(id, "22".repeat(32));
                assert_eq!(amount_msat, 1_000);
                assert_eq!(payment_hash, "33".repeat(32));
                assert_eq!(cltv_expiry, 400);
                assert!(decision.contains("undecided"), "{decision}");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(
            handler.get_and_clear_pending_msg().is_empty(),
            "no reply is sent"
        );

        // Our invoice, but nothing to price it with yet.
        handler.remember_invoice(hash, Some(1_000_000_000));
        handler
            .handle_custom_message(
                PhoenixLspMessage::WillAddHtlc(proposal(hash, 1_000_000_000)),
                lsp_id(),
            )
            .unwrap();
        assert_eq!(
            handler.snapshot().pending[0].decision,
            WillAddHtlcDecision::MissingFeerates
        );
        handler.handle_custom_message(feerates(), lsp_id()).unwrap();
        handler
            .handle_custom_message(
                PhoenixLspMessage::WillAddHtlc(proposal(hash, 1_000_000_000)),
                lsp_id(),
            )
            .unwrap();
        assert_eq!(
            handler.snapshot().pending[0].decision,
            WillAddHtlcDecision::MissingFundingRates
        );

        // With the rate card the policy decides: 1000 sat/kw * 400 / 1000 =
        // 400 sat mining, 1000 base + 1000 proportional + 2000 creation.
        handler.set_will_fund_rates(WillFundRates {
            funding_rates: vec![FundingRate {
                min_amount_sat: 10_000,
                max_amount_sat: 1_000_000,
                funding_weight: 400,
                fee_proportional_bps: 100,
                fee_base_sat: 1_000,
                channel_creation_fee_sat: 2_000,
            }],
            payment_types: vec![PaymentType::FromFutureHtlc],
        });
        handler
            .handle_custom_message(
                PhoenixLspMessage::WillAddHtlc(proposal(hash, 1_000_000_000)),
                lsp_id(),
            )
            .unwrap();
        assert_eq!(
            handler.snapshot().pending[0].decision,
            WillAddHtlcDecision::Policy(PolicyDecision::Accept)
        );
        // A small payment cannot pay 4_400 sat of fees.
        handler.remember_invoice(hash, Some(1_000));
        handler
            .handle_custom_message(
                PhoenixLspMessage::WillAddHtlc(proposal(hash, 1_000)),
                lsp_id(),
            )
            .unwrap();
        assert!(matches!(
            handler.snapshot().pending[0].decision,
            WillAddHtlcDecision::Policy(PolicyDecision::Reject(RejectReason::OverFeeCredit { .. }))
        ));

        // A cancel from the LSP clears the proposal and is surfaced.
        while events.try_recv().is_ok() {}
        handler
            .handle_custom_message(
                PhoenixLspMessage::CancelOnTheFlyFunding(CancelOnTheFlyFunding {
                    channel_id: lampo_common::ldk::ln::types::ChannelId([0x66; 32]),
                    payment_hashes: vec![hash],
                    reason: b"too slow".to_vec(),
                }),
                lsp_id(),
            )
            .unwrap();
        assert!(handler.snapshot().pending.is_empty());
        match events.try_recv().unwrap() {
            Event::Lightning(LightningEvent::PhoenixLspFundingCancelled {
                payment_hashes,
                reason,
                ..
            }) => {
                assert_eq!(payment_hashes, vec!["33".repeat(32)]);
                assert_eq!(reason, "too slow");
            }
            other => panic!("unexpected {other:?}"),
        }
        handler.forget_invoice(&hash);
    }

    #[test]
    fn queues_requests_only_while_the_lsp_is_connected() {
        let (handler, mut events) = make_handler(Some(LSP), policy());
        assert!(handler
            .send_dns_address_request(vec![1, 2, 3], "en")
            .is_err());
        connect(&handler, lsp_id(), &[561]);
        let _ = events.try_recv();
        handler
            .send_dns_address_request(vec![1, 2, 3], "en")
            .unwrap();
        handler
            .send_add_fee_credit(PaymentPreimage([0x77; 32]))
            .unwrap();
        let queued = handler.get_and_clear_pending_msg();
        assert_eq!(queued.len(), 2);
        assert_eq!(queued[0].0, lsp_id());
        assert!(matches!(
            queued[0].1,
            PhoenixLspMessage::DnsAddressRequest(_)
        ));
        assert!(matches!(queued[1].1, PhoenixLspMessage::AddFeeCredit(_)));
        assert!(handler.get_and_clear_pending_msg().is_empty());

        handler
            .handle_custom_message(
                PhoenixLspMessage::DnsAddressResponse(DnsAddressResponse {
                    chain_hash: regtest(),
                    address: "alice@phoenix.io".to_owned(),
                }),
                lsp_id(),
            )
            .unwrap();
        match events.try_recv().unwrap() {
            Event::Lightning(LightningEvent::PhoenixLspDnsAddress { address }) => {
                assert_eq!(address, "alice@phoenix.io");
            }
            other => panic!("unexpected {other:?}"),
        }

        let (idle, _events) = make_handler(None, policy());
        assert!(idle
            .send_add_fee_credit(PaymentPreimage([0x77; 32]))
            .is_err());
    }
}
