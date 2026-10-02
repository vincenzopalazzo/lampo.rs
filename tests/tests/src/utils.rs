//! Test Utils
use std::str::FromStr;
use std::sync::Arc;

use lampo_testing::prelude::*;

use lampo_common::error;

pub fn fund_wallet(btc: Arc<BtcNode>, addr: &str, blocks: u64) -> error::Result<String> {
    // mine some bitcoin inside the lampo address
    let address = lampo_common::bitcoin::Address::from_str(addr)
        .unwrap()
        .assume_checked();
    let _ = btc
        .client
        .generate_to_address(blocks as usize, &address)
        .unwrap();

    Ok(address.to_string())
}
