use bitcoin::secp256k1::PublicKey;
use lightning::events::HTLCDestination;
use lightning::ln::{channelmanager::InterceptId, ChannelId, PaymentHash};
use lightning_liquidity::{lsps0::ser::RequestId, lsps2::msgs::OpeningFeeParams};

#[derive(Debug, Clone)]
pub enum LiquidityEvent {
    OpenParamsReady {
        counterparty_node_id: PublicKey,
        opening_fee_params_menu: Vec<OpeningFeeParams>,
    },
    InvoiceparamsReady {
        counterparty_node_id: PublicKey,
        intercept_scid: u64,
        cltv_expiry_delta: u32,
    },
    BuyRequest {
        request_id: RequestId,
        counterparty_node_id: PublicKey,
        opening_fee_params: OpeningFeeParams,
        payment_size_msat: Option<u64>,
    },
    Geinfo {
        request_id: RequestId,
        counterparty_node_id: PublicKey,
        token: Option<String>,
    },
    OpenChannel {
        their_network_key: PublicKey,
        amt_to_forward_msat: u64,
        opening_fee_msat: u64,
        user_channel_id: u128,
        intercept_scid: u64,
    },
    HTLCHandlingFailed {
        prev_channel_id: ChannelId,
        failed_next_destination: HTLCDestination,
    },
    HTLCIntercepted {
        intercept_id: InterceptId,
        requested_next_hop_scid: u64,
        payment_hash: PaymentHash,
        inbound_amount_msat: u64,
        expected_outbound_amount_msat: u64,
    },
    PaymentForwarded {
        prev_channel_id: Option<ChannelId>,
        next_channel_id: Option<ChannelId>,
        prev_user_channel_id: Option<u128>,
        next_user_channel_id: Option<u128>,
        total_fee_earned_msat: Option<u64>,
        skimmed_fee_msat: Option<u64>,
        claim_from_onchain_tx: bool,
        outbound_amount_forwarded_msat: Option<u64>,
    },
}
