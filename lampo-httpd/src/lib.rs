mod commands;
mod handler;
mod rest_protocol;

use std::net::ToSocketAddrs;
use std::{fmt::Display, sync::Arc};

use actix::{web, HttpResponseWrapper, OpenApiExt};
use actix_web::{App, HttpResponse, HttpServer};
use commands::inventory::{rest_get_info, rest_json_network_channels};
use commands::offchain::{rest_json_decode_invoice, rest_json_invoice, rest_json_pay};
use commands::peer::{
    rest_json_close_channel, rest_json_connect, rest_json_list_channels, rest_json_open_channel,
};
use paperclip::actix::{self, CreatedJson};

use lampo_common::error;
use lampod::LampoDaemon;

/// Result type for json responses
pub type ResultJson<T> = std::result::Result<CreatedJson<T>, actix_web::Error>;

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
    let server = HttpServer::new(move || {
        let state = AppState::new(lampod.clone(), host_str.clone(), open_api_url.clone()).unwrap();
        // FIXME: It is possible to avoid mapping the service in here?
        // it ispossible to init the app outside the callback and
        // use the macros to do add services?
        App::new()
            .app_data(web::Data::new(state))
            .wrap_api()
            .service(swagger_api)
            .service(rest_get_info)
            .service(rest_json_network_channels)
            .service(rest_json_connect)
            .service(rest_json_open_channel)
            .service(rest_json_close_channel)
            .service(rest_json_list_channels)
            .service(rest_json_invoice)
            .service(rest_json_decode_invoice)
            .service(rest_json_pay)
            .with_json_spec_at("/api/v1")
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
       post!($name, request: json::Value, response: $res_ty);
    };
    ($name:ident, request: $req_ty:ty, response: $res_ty:ty) => {
        paste!{
            #[actix::api_v2_operation]
            #[actix::post(concat!("/", stringify!($name).to_lowercase()))]
            pub async fn [<rest_ $name>](
                state: web::Data<AppState>,
                body: Json<json::Value>,
            ) -> ResultJson<$res_ty> {
                let request = json::from_value::<$req_ty>(body.into_inner());
                if let Err(err) = request {
                    return Err(actix_web::error::ErrorBadRequest(err));
                }
                let request = request.unwrap();
                let request = json::to_value(&request).unwrap();
                let response = $name(&state.lampod, &request);
                if let Err(err) = response {
                    return Err(actix_web::error::ErrorInternalServerError(err));
                }
                let response = json::from_value::<$res_ty>(response.unwrap());
                let response = response.unwrap();
                Ok(CreatedJson(response))
            }
        }
    };
}
