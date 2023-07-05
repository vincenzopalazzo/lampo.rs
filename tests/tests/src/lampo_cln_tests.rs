use lampo_common::error;

use lampo_testing::prelude::*;
use lampo_testing::LampoTesting;

use crate::init;

#[tokio::test]
pub async fn init_coffee_test() -> error::Result<()> {
    init();
    let cln = cln::Node::tmp().await?;
    let lampo = LampoTesting::new(cln.btc());
    Ok(())
}
