use paperclip::actix::web;
use paperclip::actix::web::Json;
use paperclip::actix::{self, CreatedJson};

use lampo_common::json;
use lampod::jsonrpc::plugin::{json_plugin_start, json_plugin_stop};

use crate::{AppState, ResultJson};

#[actix::api_v2_operation]
#[actix::post("/plugin-start")]
pub async fn rest_plugin_start(
    state: web::Data<AppState>,
    body: Json<json::Value>,
) -> ResultJson<json::Value> {
    let response = json_plugin_start(&state.lampod, &body.into_inner()).await;
    match response {
        Ok(value) => Ok(CreatedJson(value)),
        Err(err) => Err(crate::JsonRPCError::from(err).into()),
    }
}

#[actix::api_v2_operation]
#[actix::post("/plugin-stop")]
pub async fn rest_plugin_stop(
    state: web::Data<AppState>,
    body: Json<json::Value>,
) -> ResultJson<json::Value> {
    let response = json_plugin_stop(&state.lampod, &body.into_inner()).await;
    match response {
        Ok(value) => Ok(CreatedJson(value)),
        Err(err) => Err(crate::JsonRPCError::from(err).into()),
    }
}
