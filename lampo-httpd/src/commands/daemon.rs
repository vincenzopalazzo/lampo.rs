use paperclip::actix::{self, web};

use lampo_common::json;

use crate::AppState;

#[actix::api_v2_operation]
#[actix::post("stop")]
pub async fn rest_stop(state: web::Data<AppState>) -> actix_web::HttpResponse {
    log::info!(target: "httpd", "Stop request received via API");
    state.lampod.shutdown();
    actix_web::HttpResponse::Ok().json(json::json!({"status": "shutting_down"}))
}
