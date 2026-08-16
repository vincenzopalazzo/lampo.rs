//! LSP plugin tests.
//!
//! These exercise the modular Approach B wiring: the type-erased custom
//! message router plus lampo-lsp composed at the testing edge. The
//! lampo-cli smoke tests talk to the HTTP API the same way a user would.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use lampo_common::error;
use lampo_common::json;
use lampo_common::model::request;
use lampo_common::model::response;
use lampo_testing::LampoTesting;

use crate::init;

fn lampo_cli_bin() -> error::Result<PathBuf> {
    // Serialize so parallel tests cannot deadlock on the cargo target lock.
    static BUILD: Mutex<()> = Mutex::new(());
    let _guard = BUILD.lock().unwrap();
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root.pop();
    let bin = root.join("target/debug/lampo-cli");
    if !bin.exists() {
        let status = Command::new("cargo")
            .args(["build", "-p", "lampo-cli"])
            .current_dir(&root)
            .status()?;
        if !status.success() {
            error::bail!("failed to build lampo-cli");
        }
    }
    Ok(bin)
}

fn lampo_cli(url: &str, args: &[&str]) -> error::Result<json::Value> {
    let bin = lampo_cli_bin()?;
    let output = Command::new(bin).arg("-u").arg(url).args(args).output()?;
    if !output.status.success() {
        error::bail!(
            "lampo-cli failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8(output.stdout)?;
    Ok(json::from_str(&stdout)?)
}

#[tokio_test_shutdown_timeout::test(2)]
pub async fn cli_getinfo_and_lsp_info_disabled_by_default() -> error::Result<()> {
    init();
    let node = LampoTesting::tmp().await?;
    let info = lampo_cli(&node.api_url, &["getinfo"])?;
    assert_eq!(info["node_id"], json::json!(node.info.node_id));

    let lsp: response::LspInfo = json::from_value(lampo_cli(&node.api_url, &["lsp-info"])?)?;
    assert!(!lsp.enabled);
    assert!(lsp.experimental);
    Ok(())
}

#[tokio_test_shutdown_timeout::test(30)]
pub async fn lsps0_list_protocols_between_two_nodes() -> error::Result<()> {
    init();
    let service = LampoTesting::tmp_with_lsp().await?;
    let client = LampoTesting::new_with_lsp(service.btc.clone()).await?;

    let _: response::Connect = client
        .lampod()
        .call(
            "connect",
            request::Connect {
                node_id: service.info.node_id.clone(),
                addr: "127.0.0.1".to_owned(),
                port: service.port,
            },
        )
        .await?;

    // Wait until the P2P connection is visible before LSPS0.
    for _ in 0..20 {
        let info: response::GetInfo = client.lampod().call("getinfo", json::json!({})).await?;
        if info.peers > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    let protocols = lampo_cli(
        &client.api_url,
        &["lsps0-list-protocols", "--node_id", &service.info.node_id],
    )?;
    assert!(
        protocols.get("protocols").is_some(),
        "expected protocols field, got {protocols}"
    );
    Ok(())
}
