//! Phoenix LSP models
pub mod request {
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    /// `phoenixlsp-dnsaddress`: ask the LSP for a BIP 353 address.
    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct PhoenixLspDnsAddress {
        /// BCP 47 language tag for the address, default `en`.
        #[serde(default)]
        pub language: Option<String>,
    }

    /// `phoenixlsp-recordpurchase`: admin entry of a liquidity purchase.
    ///
    /// Exists so funded HTLCs can be claimed end to end before this node
    /// can buy liquidity itself; every field is given explicitly.
    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct PhoenixLspRecordPurchase {
        /// Hex txid of the funding (or splice) transaction.
        pub funding_txid: String,
        /// Liquidity bought, in sat.
        pub amount_sat: u64,
        /// Mining fee owed to the LSP, in sat.
        pub mining_fee_sat: u64,
        /// Service fee owed to the LSP, in sat.
        pub service_fee_sat: u64,
        /// Liquidity ads payment type bit: 0, 128, 129 or 130.
        pub payment_type: u32,
        /// Hex payment hashes the fee is taken from, or hex preimages for
        /// payment type 129.
        #[serde(default)]
        pub payment_hashes: Vec<String>,
        /// Fee credit already applied to this purchase, in msat.
        #[serde(default)]
        pub fee_credit_used_msat: u64,
        /// Unix seconds; defaults to now.
        #[serde(default)]
        pub created_at: Option<u64>,
    }
}

pub mod response {
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    /// A liquidity purchase this node paid, or will pay, the LSP for.
    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Apiv2Schema)]
    pub struct PhoenixLspPurchase {
        pub funding_txid: String,
        pub amount_sat: u64,
        pub mining_fee_sat: u64,
        pub service_fee_sat: u64,
        /// Liquidity ads payment type bit: 0, 128, 129 or 130.
        pub payment_type: u32,
        /// Hex payment hashes, or hex preimages for payment type 129.
        #[serde(default)]
        pub payment_hashes: Vec<String>,
        #[serde(default)]
        pub fee_credit_used_msat: u64,
        pub created_at: u64,
    }

    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Apiv2Schema)]
    pub struct PhoenixLspFeerateRange {
        pub min: u32,
        pub max: u32,
    }

    /// The last `recommended_feerates` the LSP sent, in sat per kw.
    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Apiv2Schema)]
    pub struct PhoenixLspFeerates {
        pub funding_feerate: u32,
        pub commitment_feerate: u32,
        pub funding_feerate_range: Option<PhoenixLspFeerateRange>,
        pub commitment_feerate_range: Option<PhoenixLspFeerateRange>,
    }

    /// What the LSP advertised in its `init`.
    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Apiv2Schema)]
    pub struct PhoenixLspFeatures {
        /// Bit 560 or 561.
        pub on_the_fly_funding: bool,
        /// Bit 562 or 563.
        pub funding_fee_credit: bool,
        /// Bit 128 or 129.
        pub zero_reserve_channels: bool,
        /// Every feature bit set in the LSP's init features.
        pub bits: Vec<u16>,
    }

    /// A `will_add_htlc` the LSP proposed and this node has not acted on.
    #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Apiv2Schema)]
    pub struct PhoenixLspPendingHtlc {
        /// Hex id of the proposal.
        pub id: String,
        pub amount_msat: u64,
        /// Hex payment hash.
        pub payment_hash: String,
        pub cltv_expiry: u32,
        /// Unix seconds when the proposal arrived.
        pub received_at: u64,
        /// Liquidity policy decision for this proposal.
        pub decision: String,
    }

    /// `phoenixlsp-info`
    #[derive(Serialize, Deserialize, Debug, Clone, Apiv2Schema)]
    pub struct PhoenixLspInfo {
        pub configured: bool,
        pub node_id: Option<String>,
        /// `HOST:PORT` the LSP is dialled at.
        pub address: Option<String>,
        pub connected: bool,
        pub lsp_features: Option<PhoenixLspFeatures>,
        pub feerates: Option<PhoenixLspFeerates>,
        pub fee_credit_msat: u64,
        pub pending_htlcs: Vec<PhoenixLspPendingHtlc>,
        pub purchases: Vec<PhoenixLspPurchase>,
    }

    /// `phoenixlsp-dnsaddress`
    #[derive(Serialize, Deserialize, Debug, Clone, Apiv2Schema)]
    pub struct PhoenixLspDnsAddress {
        /// The BIP 353 address, `user@domain`.
        pub address: String,
    }
}
