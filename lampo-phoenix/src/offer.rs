//! A BOLT 12 offer reachable through the LSP.

use std::sync::Arc;

use lampo_common::bitcoin::secp256k1::{PublicKey, Secp256k1, Signing, Verification};
use lampo_common::error;
use lampo_common::ldk::blinded_path::message::{
    BlindedMessagePath, MessageContext, MessageForwardNode,
};
use lampo_common::ldk::offers::offer::Offer;
use lampo_common::ldk::onion_message::messenger::{Destination, MessageRouter, OnionMessagePath};
use lampo_common::ldk::sign::ReceiveAuthKey;
use lampo_common::signer::LampoSigner;
use lampo_common::types::LampoChannel;

/// An offer whose blinded path starts at `intro_node`. The introduction
/// node needs no channel with this node: the LSP publishes the offer under
/// a BIP 353 name and forwards invoice requests to us.
pub fn offer_via_introduction_node(
    channel_manager: &LampoChannel,
    signer: Arc<dyn LampoSigner>,
    intro_node: PublicKey,
) -> error::Result<Offer> {
    let router = IntroductionNodeRouter { intro_node, signer };
    channel_manager
        .create_offer_builder_using_router(&router)
        .map_err(|err| error::anyhow!("offer via `{intro_node}`: {err:?}"))?
        .build()
        .map_err(|err| error::anyhow!("build offer via `{intro_node}`: {err:?}"))
}

/// Blinds every path through one fixed introduction node, whether or not
/// it is connected or in the graph. Only used to build offers, which never
/// asks for an onion message route.
struct IntroductionNodeRouter {
    intro_node: PublicKey,
    signer: Arc<dyn LampoSigner>,
}

impl MessageRouter for IntroductionNodeRouter {
    fn find_path(
        &self,
        _sender: PublicKey,
        _peers: Vec<PublicKey>,
        _destination: Destination,
    ) -> Result<OnionMessagePath, ()> {
        Err(())
    }

    fn create_blinded_paths<T: Signing + Verification>(
        &self,
        recipient: PublicKey,
        local_node_receive_key: ReceiveAuthKey,
        context: MessageContext,
        _peers: Vec<MessageForwardNode>,
        secp_ctx: &Secp256k1<T>,
    ) -> Result<Vec<BlindedMessagePath>, ()> {
        let hop = MessageForwardNode {
            node_id: self.intro_node,
            short_channel_id: None,
        };
        Ok(vec![BlindedMessagePath::new(
            &[hop],
            recipient,
            local_node_receive_key,
            context,
            true,
            self.signer.clone(),
            secp_ctx,
        )])
    }
}
