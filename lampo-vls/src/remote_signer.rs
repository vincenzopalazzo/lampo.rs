use std::sync::Arc;

use lampo_common::conf::LampoConf;
use lampo_common::error;

use crate::InnerSpendableInterface;

/// Create the VLS remote signer
pub async fn build_remote(config: Arc<LampoConf>) -> error::Result<InnerSpendableInterface> {
    unimplemented!()
}
