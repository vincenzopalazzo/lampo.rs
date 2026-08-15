mod commands;
pub mod handler;
mod rest_protocol;

use std::net::ToSocketAddrs;
use std::{fmt::Display, sync::Arc};

use actix::{web, HttpResponseWrapper, OpenApiExt};
use actix_web::body::MessageBody;
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::middleware::{from_fn, Next};
use actix_web::{App, Error, HttpResponse, HttpServer, ResponseError};
use paperclip::actix::{self, CreatedJson};

use lampo_common::error;
use lampo_common::json;
use lampod::LampoDaemon;

use commands::daemon::rest_stop;
use commands::inventory::{rest_funds, rest_getinfo, rest_networkchannels};
use commands::offchain::{rest_decode, rest_invoice, rest_keysend, rest_pay};
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

/// DNS-rebinding guard configuration shared with the [`reject_rebinding`]
/// middleware.
#[derive(Clone)]
struct HostGuard {
    /// Host part of the address the API is bound to (no port).
    bind_host: String,
    /// Whether to enforce the `Host` header check. Empty bind hosts skip
    /// it; wildcard binds still enforce, but allow IP-literal Host headers
    /// rather than matching `0.0.0.0` / `::` (no client sends those).
    enforce: bool,
}

impl HostGuard {
    fn is_ip(host: &str) -> bool {
        host.parse::<std::net::IpAddr>().is_ok()
    }

    fn is_wildcard(host: &str) -> bool {
        host == "0.0.0.0" || host == "::"
    }

    fn is_loopback(host: &str) -> bool {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false)
    }

    /// Strip an optional `:port` from a Host header value without mangling
    /// bracketed IPv6 literals (`[::1]` / `[::1]:7979`).
    fn host_without_port(header: &str) -> &str {
        if let Some(rest) = header.strip_prefix('[') {
            if let Some(end) = rest.find(']') {
                return &rest[..end];
            }
        }
        header.rsplit_once(':').map(|(h, _)| h).unwrap_or(header)
    }

    /// Compare Host names: IP literals via parsed equality (IPv6 hex is
    /// case-insensitive), DNS names via ASCII case-insensitive equality.
    fn hosts_equal(a: &str, b: &str) -> bool {
        match (a.parse::<std::net::IpAddr>(), b.parse::<std::net::IpAddr>()) {
            (Ok(ia), Ok(ib)) => ia == ib,
            _ => a.eq_ignore_ascii_case(b),
        }
    }

    /// Returns whether a request's `Host` header is allowed to reach the API.
    fn allows(&self, req: &ServiceRequest) -> bool {
        if !self.enforce {
            return true;
        }
        let Some(header) = req
            .headers()
            .get(actix_web::http::header::HOST)
            .and_then(|value| value.to_str().ok())
        else {
            // A missing/undecodable Host header on an enforced bind is not a
            // legitimate client; reject it.
            return false;
        };
        let host = Self::host_without_port(header);
        if Self::hosts_equal(host, &self.bind_host) {
            return true;
        }
        // Loopback bind (`127.0.0.1`, `::1`, or `localhost`): allow the
        // other loopback aliases. Binding `localhost` must still accept
        // `Host: 127.0.0.1` (and vice versa).
        if Self::is_loopback(&self.bind_host) && Self::is_loopback(host) {
            return true;
        }
        // Wildcard bind: a domain Host is the DNS-rebinding signature.
        // Literal IPs (and localhost) are how a real client addresses an
        // interface-wildcard socket, so they stay allowed.
        if Self::is_wildcard(&self.bind_host) && (Self::is_ip(host) || Self::is_loopback(host)) {
            return true;
        }
        false
    }
}

/// Reject requests whose `Host` header does not match the bound address.
///
/// This is the server-side defence against DNS-rebinding: the loopback-only
/// default bind is the API's *only* access control, and a rebinding attack
/// turns any page the operator visits into a same-origin client of the
/// unauthenticated control plane. actix routes purely on `(method, path)` and
/// never inspects `Host`, so without this middleware `http://evil.tld:7979`
/// re-resolved to `127.0.0.1` reaches every fund-moving endpoint. A rebound
/// request still carries the attacker's `Host`, so matching it against the
/// bind address blocks the attack while leaving genuine loopback clients
/// (`127.0.0.1`, `localhost`) untouched.
async fn reject_rebinding(
    guard: web::Data<HostGuard>,
    req: ServiceRequest,
    next: Next<impl MessageBody + 'static>,
) -> Result<ServiceResponse<impl MessageBody + 'static>, Error> {
    if guard.allows(&req) {
        Ok(next.call(req).await?.map_into_boxed_body())
    } else {
        let host = req
            .headers()
            .get(actix_web::http::header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("<none>")
            .to_owned();
        log::warn!(
            target: "httpd",
            "rejecting request with disallowed Host header `{host}` (DNS-rebinding guard)"
        );
        Ok(req.into_response(HttpResponse::Forbidden().finish()))
    }
}

pub async fn run<T: ToSocketAddrs + Display>(
    lampod: Arc<LampoDaemon>,
    host: T,
    open_api_url: String,
) -> error::Result<()> {
    let host_str = format!("{host}");
    log::info!("httpd api running on `{host_str}`");

    // Host part of the bind address, used by the DNS-rebinding guard.
    let bind_host = host_str
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(host_str.as_str())
        .trim_matches(|c| c == '[' || c == ']')
        .to_string();
    // Enforce even on wildcard binds: skipping the check is how DNS
    // rebinding reaches a LAN-exposed unauthenticated API. The matcher
    // treats wildcards as "IP-literal Host only", not "allow anything".
    let enforce_host = !bind_host.is_empty();

    let server = HttpServer::new(move || {
        let state = AppState::new(lampod.clone(), host_str.clone(), open_api_url.clone()).unwrap();
        let host_guard = HostGuard {
            bind_host: bind_host.clone(),
            enforce: enforce_host,
        };
        // FIXME: It is possible to avoid mapping the service in here?
        // it ispossible to init the app outside the callback and
        // use the macros to do add services?
        App::new()
            .app_data(web::Data::new(state))
            .app_data(web::Data::new(host_guard))
            .wrap(from_fn(reject_rebinding))
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
            .service(rest_keysend)
            .service(rest_funds)
            .service(rest_new_addr)
            .service(rest_stop)
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

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::middleware::from_fn;
    use actix_web::{test, web, App, HttpResponse};

    use super::{reject_rebinding, HostGuard};

    async fn ok_handler() -> HttpResponse {
        HttpResponse::Ok().finish()
    }

    async fn status_for(guard: HostGuard, host: Option<&str>) -> StatusCode {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(guard))
                .wrap(from_fn(reject_rebinding))
                .route("/getinfo", web::post().to(ok_handler)),
        )
        .await;

        let mut req = test::TestRequest::post().uri("/getinfo");
        if let Some(host) = host {
            req = req.insert_header(("Host", host));
        }
        test::call_service(&app, req.to_request()).await.status()
    }

    fn loopback_guard() -> HostGuard {
        HostGuard {
            bind_host: "127.0.0.1".to_string(),
            enforce: true,
        }
    }

    // A rebound origin (`http://evil.tld:7979` re-resolved to 127.0.0.1) still
    // carries the attacker's Host header, so it must be rejected.
    #[actix_web::test]
    async fn rejects_foreign_host() {
        assert_eq!(
            status_for(loopback_guard(), Some("evil.attacker.tld")).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_for(loopback_guard(), Some("evil.attacker.tld:7979")).await,
            StatusCode::FORBIDDEN
        );
    }

    // A missing Host header on an enforced bind is not a legitimate client.
    #[actix_web::test]
    async fn rejects_missing_host() {
        assert_eq!(
            status_for(loopback_guard(), None).await,
            StatusCode::FORBIDDEN
        );
    }

    // Genuine loopback clients keep working (bind IP, `localhost`, `::1`).
    #[actix_web::test]
    async fn allows_loopback_hosts() {
        for host in [
            "127.0.0.1",
            "127.0.0.1:7979",
            "localhost:7979",
            "[::1]",
            "[::1]:7979",
        ] {
            assert_eq!(
                status_for(loopback_guard(), Some(host)).await,
                StatusCode::OK,
                "host `{host}` should be allowed"
            );
        }
    }

    // DNS Host comparison is ASCII case-insensitive (RFC 4343).
    #[actix_web::test]
    async fn allows_case_insensitive_dns_hosts() {
        let guard = HostGuard {
            bind_host: "localhost".to_string(),
            enforce: true,
        };
        for host in ["LOCALHOST", "LocalHost:7979", "localhost"] {
            assert_eq!(
                status_for(guard.clone(), Some(host)).await,
                StatusCode::OK,
                "host `{host}` should be allowed"
            );
        }
        let named = HostGuard {
            bind_host: "api.example".to_string(),
            enforce: true,
        };
        assert_eq!(
            status_for(named.clone(), Some("API.EXAMPLE:7979")).await,
            StatusCode::OK
        );
        assert_eq!(
            status_for(named, Some("evil.EXAMPLE")).await,
            StatusCode::FORBIDDEN
        );
    }

    // Binding `localhost` must still accept loopback IP Host headers.
    #[actix_web::test]
    async fn localhost_bind_allows_loopback_ips() {
        let guard = HostGuard {
            bind_host: "localhost".to_string(),
            enforce: true,
        };
        for host in ["localhost:7979", "127.0.0.1:7979", "[::1]:7979"] {
            assert_eq!(
                status_for(guard.clone(), Some(host)).await,
                StatusCode::OK,
                "host `{host}` should be allowed"
            );
        }
        assert_eq!(
            status_for(guard, Some("evil.attacker.tld")).await,
            StatusCode::FORBIDDEN
        );
    }

    // Wildcard binds still reject domain Host headers (DNS rebinding)
    // and only accept IP literals / localhost.
    #[actix_web::test]
    async fn wildcard_bind_rejects_domain_hosts() {
        let guard = HostGuard {
            bind_host: "0.0.0.0".to_string(),
            enforce: true,
        };
        assert_eq!(
            status_for(guard.clone(), Some("anything.tld")).await,
            StatusCode::FORBIDDEN
        );
        for host in ["192.168.1.5:7979", "127.0.0.1:7979", "localhost:7979"] {
            assert_eq!(
                status_for(guard.clone(), Some(host)).await,
                StatusCode::OK,
                "host `{host}` should be allowed on a wildcard bind"
            );
        }
    }
}
