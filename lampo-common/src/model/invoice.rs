//! Model for the invoice stuff

pub mod request {
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct GenerateInvoice {
        pub amount_msat: Option<u64>,
        pub description: String,
        pub expiring_in: Option<u32>,
    }

    #[derive(Serialize, Deserialize, Debug)]
    pub struct GenerateOffer {
        pub amount_msat: Option<u64>,
        pub description: Option<String>,
    }

    #[derive(Debug, Serialize, Deserialize, Apiv2Schema)]
    pub struct DecodeInvoice {
        pub invoice_str: String,
    }

    #[derive(Serialize, Deserialize, Apiv2Schema)]
    pub struct Pay {
        pub invoice_str: String,
        pub amount: Option<u64>,
        pub bolt12: Option<Bolt12Pay>,
    }

    #[derive(Serialize, Deserialize, Apiv2Schema)]
    pub struct Bolt12Pay {
        pub payer_note: Option<String>,
    }
}

pub mod response {
    use std::vec::Vec;

    use bitcoin::{secp256k1::PublicKey, Network};
    use lightning::offers::offer::Offer as LDKOffer;
    use lightning::routing::router::RouteHop;
    use paperclip::actix::Apiv2Schema;
    use serde::{Deserialize, Serialize};

    use crate::ldk;

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct Invoice {
        pub bolt11: String,
    }

    #[derive(Serialize, Deserialize, Debug)]
    pub struct Offer {
        pub bolt12: String,
        pub metadata: Option<String>,
        pub metadata_pubkey: Option<PublicKey>,
    }

    impl From<ldk::offers::offer::Offer> for Offer {
        fn from(value: ldk::offers::offer::Offer) -> Self {
            Self {
                bolt12: value.to_string(),
                metadata: value.metadata().map(hex::encode),
                metadata_pubkey: value.issuer_signing_pubkey(),
            }
        }
    }

    #[derive(Debug, Serialize, Deserialize, Apiv2Schema)]
    pub struct Bolt11InvoiceInfo {
        pub issuer_id: Option<String>,
        pub expiry_time: Option<u64>,
        pub description: Option<String>,
        pub routes: Vec<String>,
        pub hints: Vec<String>,
        pub network: String,
        pub amount_msat: Option<u64>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Bolt12InvoiceInfo {
        pub issuer_id: Option<String>,
        pub offer_id: String,
        pub offer_chains: Vec<String>,
        pub description: Option<String>,
        pub offer_paths: Vec<BlindedPath>,
        pub network: String,
    }

    impl From<LDKOffer> for Bolt12InvoiceInfo {
        fn from(offer: LDKOffer) -> Self {
            let chains = offer
                .chains()
                .iter()
                .map(|chain| chain.to_string())
                .collect::<Vec<String>>();

            // Reference: https://github.com/lightning/bolts/blob/master/12-offer-encoding.md#requirements-for-offers
            // if the chain for the invoice is not solely bitcoin:
            // MUST specify offer_chains the offer is valid for.
            // otherwise:
            // SHOULD omit offer_chains, implying that bitcoin is only chain.
            let network = offer
                .chains()
                .first()
                .and_then(|hash| Network::from_chain_hash(*hash));

            let paths = offer
                .paths()
                .to_vec()
                .iter()
                .map(|path| BlindedPath {
                    blinded_hops: path
                        .blinded_hops()
                        .iter()
                        .map(|node| node.blinded_node_id.to_string())
                        .collect::<Vec<String>>(),
                    blinding_points: path.blinding_point().to_string(),
                })
                .collect::<Vec<BlindedPath>>();

            let offer_id = hex::encode(offer.id().0);
            let desc = offer.description().map(|desc| desc.to_string());
            let issuer_id = offer.issuer().map(|id| id.to_string());

            Bolt12InvoiceInfo {
                offer_id,
                network: network.unwrap().to_string(),
                description: desc,
                offer_chains: chains,
                offer_paths: paths,
                issuer_id,
            }
        }
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct BlindedPath {
        pub blinded_hops: Vec<String>,
        pub blinding_points: String,
    }

    #[derive(Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct PayResult {
        pub path: Vec<PaymentHop>,
        pub payment_hash: Option<String>,
        pub state: PaymentState,
        // FIXME: missing payment preimage
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Apiv2Schema)]
    pub enum PaymentState {
        Success,
        Pending,
        Failure,
    }

    #[derive(Clone, Serialize, Deserialize, Debug, Apiv2Schema)]
    pub struct PaymentHop {
        pub node_id: String,
        pub short_channel_id: u64,
        pub hop_fee_msat: u64,
        pub cltv_expiry_delta: u32,
        pub private_hop: bool,
    }

    impl From<RouteHop> for PaymentHop {
        fn from(value: RouteHop) -> Self {
            Self {
                node_id: value.pubkey.to_string(),
                short_channel_id: value.short_channel_id,
                hop_fee_msat: value.fee_msat,
                cltv_expiry_delta: value.cltv_expiry_delta,
                private_hop: value.maybe_announced_channel,
            }
        }
    }
}
