//! Nodes whose keys live in a Validating Lightning Signer.
//!
//! Compiled only with `--features vls`, and then they must run: they need
//! `VLSD_EXE` and `REMOTE_HSMD_SOCKET_EXE` to point at VLS binaries built
//! from `validating-lightning-signer` (`vlsd` and `remote_hsmd_socket`), and
//! fail rather than skip without them, so a misconfigured CI cannot pass.
use std::sync::Arc;

use lampo_common::error;
use lampo_common::json;
use lampo_common::model::{request, response};
use lampo_testing::{async_wait, LampoTesting, VlsBinaries};

use crate::init;

fn vls_binaries() -> VlsBinaries {
    VlsBinaries::from_env().expect("the vls feature needs VLSD_EXE and REMOTE_HSMD_SOCKET_EXE set")
}

/// The channel lifecycle with the keys held by vlsd: the VLS node opens the
/// channel, pays an invoice and closes cooperatively, and the close output
/// lands in its on-chain wallet. The close pays straight to a wallet script,
/// so this passes the signer's mutual-close allowlist check but does not
/// exercise `SignWithdrawal` (that needs a force close).
#[tokio_test_shutdown_timeout::test(10)]
pub async fn vls_open_pay_and_close_to_wallet() -> error::Result<()> {
    init();
    let bins = vls_binaries();
    let node1 = LampoTesting::tmp_with_vls(bins).await?;
    assert!(node1.uses_vls());
    let btc = node1.btc.clone();
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);

    const CHANNEL_SAT: u64 = 1_000_000;
    node1.fund_channel_with(node2.clone(), CHANNEL_SAT).await?;

    let invoice: response::Invoice = node2
        .lampod()
        .call(
            "invoice",
            request::GenerateInvoice {
                description: "paid with keys in vlsd".to_owned(),
                amount_msat: Some(100_000),
                expiring_in: None,
            },
        )
        .await?;
    let pay: response::PayResult = node1
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: invoice.bolt11,
                amount: None,
                max_fee_msat: None,
                bolt12: None,
                timeout: Default::default(),
                timeout_secs: None,
            },
        )
        .await?;
    assert!(
        pay.payment_preimage.is_some(),
        "payment must settle: {pay:?}"
    );

    let in_close_range = |utxo: &response::Utxo| {
        let sat = utxo.amount_msat / 1000;
        sat > CHANNEL_SAT / 2 && sat < CHANNEL_SAT
    };
    let funds: response::Utxos = node1.lampod().call("funds", json::json!({})).await?;
    assert!(!funds.transactions.iter().any(in_close_range));

    let close: response::CloseChannel = node1
        .lampod()
        .call(
            "close",
            request::CloseChannel {
                node_id: node2.info.node_id.clone(),
                channel_id: None,
                force: false,
            },
        )
        .await?;
    log::info!(target: &node1.info.node_id, "channel closed: {close:?}");

    // Keep mining until the close confirms and the wallet sees its output.
    async_wait!(
        async {
            node1.fund_wallet(2).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let funds: response::Utxos =
                node1.lampod().call("funds", json::json!({})).await.unwrap();
            if funds.transactions.iter().any(in_close_range) {
                Ok(())
            } else {
                Err(())
            }
        },
        10
    );
    Ok(())
}

/// The in-memory node opens towards the VLS node, so the VLS side runs the
/// inbound path: `validate_holder_commitment` before any signing call,
/// which exercises the deferred `SetupChannel` replay.
#[tokio_test_shutdown_timeout::test(10)]
pub async fn vls_accepts_inbound_channel_and_receives() -> error::Result<()> {
    init();
    let bins = vls_binaries();
    let node1 = LampoTesting::tmp().await?;
    let btc = node1.btc.clone();
    let node2 = Arc::new(LampoTesting::new_with_vls(btc.clone(), bins).await?);
    assert!(node2.uses_vls());

    node1.fund_channel_with(node2.clone(), 1_000_000).await?;

    let invoice: response::Invoice = node2
        .lampod()
        .call(
            "invoice",
            request::GenerateInvoice {
                description: "received with keys in vlsd".to_owned(),
                amount_msat: Some(50_000),
                expiring_in: None,
            },
        )
        .await?;
    let pay: response::PayResult = node1
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: invoice.bolt11,
                amount: None,
                max_fee_msat: None,
                bolt12: None,
                timeout: Default::default(),
                timeout_secs: None,
            },
        )
        .await?;
    assert!(
        pay.payment_preimage.is_some(),
        "payment must settle: {pay:?}"
    );
    Ok(())
}
