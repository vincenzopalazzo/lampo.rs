mod commands;
pub mod handler;
mod rest_protocol;

use std::net::ToSocketAddrs;
use std::{fmt::Display, sync::Arc};

use actix::{web, HttpResponseWrapper, OpenApiExt};
use actix_web::{App, HttpResponse, HttpServer, ResponseError};
use paperclip::actix::{self, CreatedJson};

use lampo_common::error;
use lampo_common::json;
use lampod::LampoDaemon;

use commands::inventory::{rest_funds, rest_getinfo, rest_networkchannels};
use commands::offchain::{rest_decode, rest_invoice, rest_pay};
use commands::onchain::rest_new_addr;
use commands::peer::{rest_channels, rest_close, rest_connect, rest_fundchannel};

use crate::commands::offchain::rest_offer;

/// Result type for json responses
pub type ResultJson<T> = std::result::Result<CreatedJson<T>, actix_web::Error>;

#[derive(Debug)]
struct JsonRPCError {
    code: i32,
    message: String,
    data: Option<json::Value>,
}

impl Display for JsonRPCError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "code: {}, message: {}", self.code, self.message)
    }
}

impl ResponseError for JsonRPCError {
    fn status_code(&self) -> actix_web::http::StatusCode {
        actix_web::http::StatusCode::INTERNAL_SERVER_ERROR
    }

    fn error_response(&self) -> actix_web::HttpResponse {
        let status_code = self.status_code().clone();
        let body = json::json!({
            "code": self.code,
            "message": self.message,
            "data": self.data,
        });
        let body = json::to_string(&body).unwrap();
        log::warn!(
            "error response from inside the ResponseError trait: {}",
            body
        );
        actix_web::HttpResponse::new(status_code).set_body(actix_web::body::BoxBody::new(
            actix_web::web::Bytes::from(body),
        ))
    }
}

impl From<lampo_common::jsonrpc::Error> for JsonRPCError {
    fn from(err: lampo_common::jsonrpc::Error) -> Self {
        match err {
            lampo_common::jsonrpc::Error::Rpc(err) => Self {
                code: err.code,
                message: err.message,
                data: err.data,
            },
            _ => Self {
                code: -1,
                message: format!("{err}"),
                data: None,
            },
        }
    }
}

/// This struct represents app state and it is pass on every
/// endpoint.
pub(crate) struct AppState {
    host: String,
    open_api_url: String,

    lampod: Arc<LampoDaemon>,
}

impl AppState {
    pub fn new(
        lampod: Arc<LampoDaemon>,
        host: String,
        open_api_url: String,
    ) -> error::Result<Self> {
        Ok(Self {
            host,
            open_api_url,
            lampod,
        })
    }
}

pub async fn run<T: ToSocketAddrs + Display>(
    lampod: Arc<LampoDaemon>,
    host: T,
    open_api_url: String,
) -> error::Result<()> {
    let host_str = format!("{host}");
    log::info!("httpd api running on `{host_str}`");

    let server = HttpServer::new(move || {
        let state = AppState::new(lampod.clone(), host_str.clone(), open_api_url.clone()).unwrap();
        // FIXME: It is possible to avoid mapping the service in here?
        // it ispossible to init the app outside the callback and
        // use the macros to do add services?
        App::new()
            .app_data(web::Data::new(state))
            .wrap_api()
            .service(swagger_api)
            .service(rest_getinfo)
            .service(rest_channels)
            .service(rest_connect)
            .service(rest_fundchannel)
            .service(rest_close)
            .service(rest_networkchannels)
            .service(rest_invoice)
            .service(rest_offer)
            .service(rest_decode)
            .service(rest_pay)
            .service(rest_funds)
            .service(rest_new_addr)
            .build()
    })
    .bind(host)?;
    server.run().await?;
    Ok(())
}

// this is just a hack to support swagger UI with https://paperclip-rs.github.io/paperclip/
// and the raw html is taken from https://github.com/swagger-api/swagger-ui/blob/master/docs/usage/installation.md#unpkg
#[actix::get("/")]
async fn swagger_api(data: web::Data<AppState>) -> HttpResponseWrapper {
    // FIXME: the url need to change here so we should support a better way
    let resp = HttpResponse::Ok().body(
        format!(r#"
<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <meta
      name="description"
      content="SwaggerUI"
    />
    <title>SwaggerUI</title>
    <link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist@4.5.0/swagger-ui.css" />
  </head>
  <body>
  <div id="swagger-ui"></div>
  <script src="https://unpkg.com/swagger-ui-dist@4.5.0/swagger-ui-bundle.js" crossorigin></script>
  <script src="https://unpkg.com/swagger-ui-dist@4.5.0/swagger-ui-standalone-preset.js" crossorigin></script>
  <script>
    window.onload = () => {{
      window.ui = SwaggerUIBundle({{
        url: '{}/api/v1',
        dom_id: '#swagger-ui',
        presets: [
          SwaggerUIBundle.presets.apis,
          SwaggerUIStandalonePreset
        ],
        layout: "StandaloneLayout",
      }});
    }};
  </script>
  </body>
</html>
"#, data.open_api_url),
    );
    HttpResponseWrapper(resp)
}

#[macro_export]
macro_rules! post {
    ($name:ident, response: $res_ty:ty) => {
        paste! {
            #[actix::api_v2_operation]
            #[actix::post($name)]
            pub async fn [<rest_$name>](
                state: web::Data<AppState>,
            ) -> ResultJson<$res_ty> {
                log::debug!(target: "httpd", "request with empty json body");
                let response = [<json_$name>](&state.lampod, &json::json!({})).await;
                if let Err(err) = response {
                    let err: crate::JsonRPCError = err.into();
                    log::error!(target: "httpd", "error from backend {}", err);
                    return Err(err.into());
                }
                let response = json::from_value::<$res_ty>(response.unwrap());
                let response = response.unwrap();
                Ok(CreatedJson(response))
            }
        }
    };
    ($name:ident, request: $req_ty:ty, response: $res_ty:ty) => {
        paste! {
            #[actix::api_v2_operation]
            #[actix::post($name)]
            pub async fn [<rest_$name>](
                state: web::Data<AppState>,
                body: Json<json::Value>,
            ) -> ResultJson<$res_ty> {
                log::debug!(target: "httpd", "request with json body {:?}", body);
                let request = json::from_value::<$req_ty>(body.into_inner());
                if let Err(err) = request {
                    let err = crate::JsonRPCError{ code: -1, message: format!("{err}"), data: None };
                    log::error!(target: "httpd", "error from backend {}", err);
                    return Err(err.into());
                }
                let request = request.unwrap();
                let request = json::to_value(&request).unwrap();
                let response = [<json_$name>](&state.lampod, &request).await;
                if let Err(err) = response {
                    let err: crate::JsonRPCError = err.into();
                    log::error!(target: "httpd", "error from backend {}", err);
                    return Err(err.into());
                }
                let response = json::from_value::<$res_ty>(response.unwrap());
                let response = response.unwrap();
                Ok(CreatedJson(response))
            }
        }
    };
}
