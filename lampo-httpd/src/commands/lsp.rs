use paperclip::actix::web;
use paperclip::actix::web::Json;
use paperclip::actix::{self, CreatedJson};

use lampo_common::json;
use lampo_common::jsonrpc::Error;
use lampo_common::model::{request, response};
use lampod::LampoDaemon;

use crate::{AppState, ResultJson};

pub async fn json_lsp_info(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    ctx.call("lsp-info", request.clone())
        .await
        .map_err(Error::from)
}

pub async fn json_lsps0_list_protocols(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    ctx.call("lsps0-list-protocols", request.clone())
        .await
        .map_err(Error::from)
}

#[actix::api_v2_operation]
#[actix::post("lsp-info")]
pub async fn rest_lsp_info(state: web::Data<AppState>) -> ResultJson<response::LspInfo> {
    log::debug!(target: "httpd", "request lsp-info");
    let response = json_lsp_info(&state.lampod, &json::json!({})).await;
    if let Err(err) = response {
        let err: crate::JsonRPCError = err.into();
        log::error!(target: "httpd", "error from backend {}", err);
        return Err(err.into());
    }
    let response = json::from_value::<response::LspInfo>(response.unwrap()).unwrap();
    Ok(CreatedJson(response))
}

#[actix::api_v2_operation]
#[actix::post("lsps0-list-protocols")]
pub async fn rest_lsps0_list_protocols(
    state: web::Data<AppState>,
    body: Json<json::Value>,
) -> ResultJson<response::Protocols> {
    log::debug!(target: "httpd", "request lsps0-list-protocols {:?}", body);
    let request = json::from_value::<request::ListProtocols>(body.into_inner());
    if let Err(err) = request {
        let err = crate::JsonRPCError {
            code: -1,
            message: format!("{err}"),
            data: None,
        };
        log::error!(target: "httpd", "error from backend {}", err);
        return Err(err.into());
    }
    let request = json::to_value(&request.unwrap()).unwrap();
    let response = json_lsps0_list_protocols(&state.lampod, &request).await;
    if let Err(err) = response {
        let err: crate::JsonRPCError = err.into();
        log::error!(target: "httpd", "error from backend {}", err);
        return Err(err.into());
    }
    let response = json::from_value::<response::Protocols>(response.unwrap()).unwrap();
    Ok(CreatedJson(response))
}
