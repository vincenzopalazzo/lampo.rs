//! LND REST smoke tests (Zeus remote-node contract).
//!
//! Official `lncli` speaks gRPC only, so these tests exercise the REST
//! surface with an HTTPS client the same way Zeus does.

use lampo_common::error;
use lampo_common::json;
use lampo_testing::prelude::*;
use lampo_testing::LampoTesting;

use crate::init;

async fn lnd_get(
    node: &LampoTesting,
    path: &str,
    macaroon_hex: Option<&str>,
) -> error::Result<(u16, json::Value)> {
    let url = format!("https://127.0.0.1:{}{path}", node.lnd_rest_port);
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()?;
    let mut req = client.get(url);
    if let Some(mac) = macaroon_hex {
        req = req.header("Grpc-Metadata-macaroon", mac);
    }
    let resp = req.send().await?;
    let status = resp.status().as_u16();
    let body = resp.text().await?;
    let value = serde_json::from_str(&body).unwrap_or(json::json!({ "raw": body }));
    Ok((status, value))
}

#[tokio_test_shutdown_timeout::test(10)]
pub async fn lnd_rest_getinfo_requires_macaroon() -> error::Result<()> {
    init();
    let node = LampoTesting::tmp().await?;

    // Wait for the HTTPS listener to accept connections.
    for _ in 0..40 {
        match lnd_get(&node, "/v1/getinfo", None).await {
            Ok((status, _)) if status == 401 => break,
            Ok((status, _)) if status < 500 => break,
            _ => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
        }
    }

    let (status, _) = lnd_get(&node, "/v1/getinfo", None).await?;
    assert_eq!(status, 401, "missing macaroon must be unauthorized");
    Ok(())
}

#[tokio_test_shutdown_timeout::test(10)]
pub async fn lnd_rest_getinfo_with_admin_macaroon() -> error::Result<()> {
    init();
    let node = LampoTesting::tmp().await?;

    let mut last = None;
    for _ in 0..40 {
        match lnd_get(&node, "/v1/getinfo", Some(&node.lnd_admin_macaroon_hex)).await {
            Ok((200, body)) => {
                last = Some(body);
                break;
            }
            other => {
                last = other.ok().map(|(_, b)| b);
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
    }

    let body = last.expect("getinfo should succeed with admin macaroon");
    assert_eq!(
        body.get("identityPubkey")
            .or_else(|| body.get("identity_pubkey"))
            .and_then(|v| v.as_str()),
        Some(node.info.node_id.as_str())
    );
    assert!(
        body.get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("0.18"),
        "version should look LND-like for Zeus feature gates: {body}"
    );
    Ok(())
}

#[tokio_test_shutdown_timeout::test(10)]
pub async fn lnd_rest_wallet_balance_smoke() -> error::Result<()> {
    init();
    let node = LampoTesting::tmp().await?;

    let mut ok = false;
    for _ in 0..40 {
        if let Ok((200, body)) = lnd_get(
            &node,
            "/v1/balance/blockchain",
            Some(&node.lnd_admin_macaroon_hex),
        )
        .await
        {
            assert!(
                body.get("confirmedBalance")
                    .or_else(|| body.get("confirmed_balance"))
                    .is_some(),
                "unexpected balance body: {body}"
            );
            ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert!(ok, "wallet balance endpoint should respond");
    Ok(())
}
