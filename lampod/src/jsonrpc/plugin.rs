//! Start and stop plugins on a running daemon.
use lampo_common::json;
use lampo_common::jsonrpc::Error;
use lampo_plugin_common::messages::InitConfig;

use crate::LampoDaemon;

pub async fn json_plugin_start(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    let path = request
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            Error::Rpc(lampo_common::jsonrpc::RpcError {
                code: -32602,
                message: "plugin-start requires `path`".to_owned(),
                data: None,
            })
        })?;
    let manager = ctx
        .handler()
        .plugin_manager()
        .await
        .ok_or_else(|| anyhow_rpc("no plugin manager"))?;
    let conf = ctx.conf();
    let init = InitConfig {
        lampo_dir: conf.path(),
        network: conf.network.to_string(),
        node_id: String::new(),
        rpc_file: format!("{}/lampo-rpc", conf.path()),
        options: json::Map::new(),
    };
    let name = manager
        .start_plugin(path, &init)
        .await
        .map_err(anyhow_rpc)?;
    Ok(json::json!({"name": name, "path": path}))
}

pub async fn json_plugin_stop(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    let name = request
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            Error::Rpc(lampo_common::jsonrpc::RpcError {
                code: -32602,
                message: "plugin-stop requires `name`".to_owned(),
                data: None,
            })
        })?;
    let manager = ctx
        .handler()
        .plugin_manager()
        .await
        .ok_or_else(|| anyhow_rpc("no plugin manager"))?;
    manager.stop_plugin(name).await.map_err(anyhow_rpc)?;
    Ok(json::json!({"name": name, "stopped": true}))
}

fn anyhow_rpc(err: impl ToString) -> Error {
    Error::Rpc(lampo_common::jsonrpc::RpcError {
        code: -1,
        message: err.to_string(),
        data: None,
    })
}
