use lampo_common::error;
use lampo_common::model::Connect;
use lampo_testing::prelude::*;
use lampo_testing::LampoTesting;

use crate::init;

#[test]
pub fn init_coffee_test() -> error::Result<()> {
    init();
    let cln = async_run!(cln::Node::tmp())?;
    let lampo = LampoTesting::new(cln.btc())?;
    let info = cln.rpc().getinfo()?;
    log::debug!("core lightning info {:?}", info);
    let response = lampo.lampod().call(
        "connect",
        Connect {
            node_id: info.id,
            addr: "127.0.0.1".to_owned(),
            port: cln.port.into(),
        },
    )?;
    log::debug!("lampo connected with cln {:?}", response);
    Ok(())
}
