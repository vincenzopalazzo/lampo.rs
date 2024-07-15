use std::sync::Arc;

use lampo_common::keys::LampoKeysManager;
use lightning_liquidity::{events::Event, LiquidityManager};

use crate::{chain::LampoChainManager, ln::LampoChannel};

type LampoLiquidity =
    LiquidityManager<Arc<LampoKeysManager>, Arc<LampoChannel>, Arc<LampoChainManager>>;

struct LampoLiquiditySource {
    lampo_liquidity_manager: Arc<LampoLiquidity>,
}

impl LampoLiquiditySource {
    fn new(liquidity: Arc<LampoLiquidity>) -> Self {
        LampoLiquiditySource {
            lampo_liquidity_manager: liquidity,
        }
    }

    pub fn clean_events(&self) {
        self.lampo_liquidity_manager.get_and_clear_pending_events();
    }

    pub async fn lsp_handler(&self) {
        match self.lampo_liquidity_manager.next_event_async().await {
            Event::LSPS0Client(..) => {
                todo!()
            }
            Event::LSPS2Client(_) => todo!(),
            Event::LSPS2Service(_) => todo!(),
        }
    }
}
