//! Test Utils
use std::str::FromStr;
use std::sync::Arc;

use lampo_testing::prelude::bitcoincore_rpc;
use lampo_testing::prelude::bitcoincore_rpc::RpcApi;
use lampo_testing::prelude::btc;

use lampo_common::error;

#[macro_export]
macro_rules! wait_cln_sync {
    ($cln:expr) => {{
        use lampo_testing::wait;

        wait!(|| {
            let Ok(cln_info) = $cln.rpc().getinfo() else {
                return Err(());
            };
            log::trace!("cln info: {:?}", cln_info);
            if cln_info.warning_bitcoind_sync.is_some() {
                return Err(());
            }

            if cln_info.warning_lightningd_sync.is_some() {
                return Err(());
            }
            let mut out = $cln.rpc().listfunds().unwrap().outputs;
            out.retain(|tx| tx.status == "confirmed");
            if out.is_empty() {
                let addr = $cln.rpc().newaddr(None).unwrap().bech32.unwrap();
                let _ = fund_wallet($cln.btc(), &addr, 6);
                return Err(());
            }

            Ok(())
        });
    }};
}

#[macro_export]
macro_rules! node {
    ($btc:expr) => {{
        let pwd = std::env!("PWD");
        let plugin_name = std::env!("PLUGIN_NAME");
        log::debug!("plugin path: {pwd}/../{plugin_name}");
        cln::Node::with_btc_and_params(
            $btc,
            &format!("--developer --experimental-offers --plugin={pwd}/target/debug/{plugin_name}"),
            "regtest",
        )
        .await?
    }};
    () => {{
        let pwd = std::env!("PWD");
        let plugin_name = std::env!("PLUGIN_NAME");
        log::debug!("plugin path: {pwd}/../{plugin_name}");
        cln::Node::with_params(
            &format!("--developer --experimental-offers --plugin={pwd}/target/debug/{plugin_name}"),
            "regtest",
        )
        .await?
    }};
}

pub fn fund_wallet(btc: Arc<btc::BtcNode>, addr: &str, blocks: u64) -> error::Result<String> {
    // mine some bitcoin inside the lampo address
    let address = bitcoincore_rpc::bitcoin::Address::from_str(addr)
        .unwrap()
        .assume_checked();
    let _ = btc.rpc().generate_to_address(blocks, &address).unwrap();

    Ok(address.to_string())
}
