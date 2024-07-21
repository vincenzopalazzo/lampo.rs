use crate::LampoLiquidityManager;
use std::ops::Deref;
use std::sync::Arc;

use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::ldk::ln::features::{InitFeatures, NodeFeatures};
use lampo_common::ldk::ln::peer_handler::CustomMessageHandler;
use lampo_common::ldk::ln::wire::CustomMessageReader;

use lightning_liquidity::lsps0::ser::RawLSPSMessage;

pub enum LampoCustomMessageHandler {
    Ignoring,
    Liquidity {
        liquidity: Arc<LampoLiquidityManager>,
    },
}

impl LampoCustomMessageHandler {
    pub(crate) fn new_liquidity(liquidity: Arc<LampoLiquidityManager>) -> Self {
        Self::Liquidity { liquidity }
    }

    pub(crate) fn new_ignoring() -> Self {
        Self::Ignoring
    }
}

impl CustomMessageReader for LampoCustomMessageHandler {
    type CustomMessage = RawLSPSMessage;

    fn read<RD: lampo_common::ldk::io::Read>(
        &self,
        message_type: u16,
        buffer: &mut RD,
    ) -> Result<Option<Self::CustomMessage>, lampo_common::ldk::ln::msgs::DecodeError> {
        match self {
            Self::Ignoring => Ok(None),
            Self::Liquidity { liquidity, .. } => {
                liquidity.liquidity_manager().read(message_type, buffer)
            }
        }
    }
}

impl CustomMessageHandler for LampoCustomMessageHandler {
    fn handle_custom_message(
        &self,
        msg: Self::CustomMessage,
        sender_node_id: &PublicKey,
    ) -> Result<(), lampo_common::ldk::ln::msgs::LightningError> {
        match self {
            Self::Ignoring => Ok(()),
            Self::Liquidity { liquidity, .. } => liquidity
                .liquidity_manager()
                .handle_custom_message(msg, sender_node_id),
        }
    }

    fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, Self::CustomMessage)> {
        match self {
            Self::Ignoring => Vec::new(),
            Self::Liquidity { liquidity, .. } => {
                liquidity.liquidity_manager().get_and_clear_pending_msg()
            }
        }
    }

    fn provided_node_features(&self) -> NodeFeatures {
        match self {
            Self::Ignoring => NodeFeatures::empty(),
            Self::Liquidity { liquidity, .. } => {
                liquidity.liquidity_manager().provided_node_features()
            }
        }
    }

    fn provided_init_features(&self, their_node_id: &PublicKey) -> InitFeatures {
        match self {
            Self::Ignoring => InitFeatures::empty(),
            Self::Liquidity { liquidity, .. } => liquidity
                .liquidity_manager()
                .provided_init_features(their_node_id),
        }
    }
}

impl Deref for LampoCustomMessageHandler {
    type Target = Self;

    fn deref(&self) -> &Self::Target {
        &self
    }
}
