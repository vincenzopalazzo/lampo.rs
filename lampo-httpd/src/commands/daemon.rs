use paperclip::actix::web::Json;
use paperclip::actix::{self, web};

use lampo_common::json;

use crate::AppState;

#[actix::api_v2_operation]
#[actix::post("stop")]
pub async fn rest_stop(
    state: web::Data<AppState>,
    // Require a JSON body so this shutdown endpoint cannot be triggered by a
    // cross-origin "simple request". `application/json` is not CORS-safelisted,
    // so a browser must send a preflight the API never answers -- closing the
    // CSRF hole that let any page the operator visited kill the daemon.
    _body: Json<json::Value>,
) -> actix_web::HttpResponse {
    log::info!(target: "httpd", "Stop request received via API");
    state.lampod.shutdown();
    actix_web::HttpResponse::Ok().json(json::json!({"status": "shutting_down"}))
}
