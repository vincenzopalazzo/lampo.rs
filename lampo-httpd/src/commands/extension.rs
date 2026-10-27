use paperclip::actix::web::{self, Json};
use paperclip::actix::{self, CreatedJson};

use lampo_common::json;

use crate::{AppState, JsonRPCError, ResultJson};

/// Methods served by the extensions registered with the daemon. Mounted
/// after every typed route, so those win first.
#[actix::api_v2_operation]
#[actix::post("/{method}")]
pub async fn rest_extension(
    state: web::Data<AppState>,
    method: web::Path<String>,
    body: Json<json::Value>,
) -> ResultJson<json::Value> {
    let method = method.into_inner();
    log::debug!(target: "httpd", "extension method `{method}` with json body {:?}", body);
    match state
        .lampod
        .call_extension(&method, &body.into_inner())
        .await
    {
        Ok(Some(response)) => Ok(CreatedJson(response)),
        Ok(None) => Err(JsonRPCError {
            code: -32601,
            message: format!("method `{method}` not found"),
            data: None,
        }
        .into()),
        Err(err) => {
            let err: JsonRPCError = err.into();
            log::error!(target: "httpd", "error from extension `{method}`: {err}");
            Err(err.into())
        }
    }
}
