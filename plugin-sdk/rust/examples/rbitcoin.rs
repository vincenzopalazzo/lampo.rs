//! rbitcoin chain backend, written as a lampo plugin.
//!
//! rbitcoin's JSON-RPC subset already speaks the methods Lampo calls.
//! What differs from Bitcoin Core is the credential: a bearer token
//! (`{datadir}/rpc.token`), an optional `user:password` cookie file, or a
//! unix socket with no HTTP header.
//!
//! The on-chain wallet does not use this process. It still needs
//! `core-url` plus `rpc-cookie-file` or `core-user` / `core-pass`.
//!
//! ```text
//! cargo build -p lampo-plugin-sdk --example lampo-rbitcoin
//! lampod-cli --plugin target/debug/examples/lampo-rbitcoin
//! ```
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lampo_plugin_sdk::Plugin;
use serde_json::{json, Value};
use tokio::sync::RwLock;

const CHAIN_METHODS: &[(&str, &str, &str)] = &[
    ("sendrawtransaction", "Broadcast a raw transaction", "hex"),
    ("getblock", "Fetch a block by hash", "hash verbosity"),
    (
        "getblockheader",
        "Fetch a block header by hash",
        "hash verbose",
    ),
    ("getblockchaininfo", "Chain tip and height", ""),
    ("estimatesmartfee", "Fee estimate in BTC/kvB", "blocks mode"),
    ("getmempoolinfo", "Mempool minimum fee", ""),
];

enum RpcAuth {
    Bearer(String),
    Basic { user: String, pass: String },
}

enum EndpointKind {
    Http { url: String, auth: RpcAuth },
    Unix { path: String },
}

struct Rbitcoin {
    kind: EndpointKind,
    client: reqwest::Client,
    next_id: AtomicU64,
}

impl Rbitcoin {
    fn from_init(params: &Value) -> Result<Self, String> {
        Ok(Self {
            kind: endpoint_from_init(params)?,
            client: reqwest::Client::new(),
            next_id: AtomicU64::new(1),
        })
    }

    async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({
            "jsonrpc": "1.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let bytes = match &self.kind {
            EndpointKind::Http { url, auth } => self.http_post(url, auth, &body).await?,
            EndpointKind::Unix { path } => {
                let payload = serde_json::to_vec(&body)
                    .map_err(|err| format!("rbitcoin `{method}` encode: {err}"))?;
                unix_post(path, &payload).await?
            }
        };
        parse_rpc_result(method, &bytes)
    }

    async fn http_post(&self, url: &str, auth: &RpcAuth, body: &Value) -> Result<Vec<u8>, String> {
        let header = match auth {
            RpcAuth::Bearer(token) => format!("Bearer {token}"),
            RpcAuth::Basic { user, pass } => {
                format!("Basic {}", basic_auth(user, pass))
            }
        };
        let response = self
            .client
            .post(url)
            .header("Authorization", header)
            .json(body)
            .send()
            .await
            .map_err(|err| {
                format!(
                    "rbitcoin transport error: {}",
                    redact_userinfo(&err.to_string())
                )
            })?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|err| format!("rbitcoin empty body: {err}"))?
            .to_vec();
        if !status.is_success() && serde_json::from_slice::<Value>(&bytes).is_err() {
            return Err(format!("rbitcoin HTTP {status}"));
        }
        Ok(bytes)
    }
}

fn endpoint_from_init(params: &Value) -> Result<EndpointKind, String> {
    let socket = opt_str(params, "rpc-socket");
    if let Some(path) = socket {
        return Ok(EndpointKind::Unix { path });
    }
    let raw_url = opt_str(params, "core-url")
        .ok_or("rbitcoin plugin: init needs options.core-url or options.rpc-socket")?;
    // Credentials belong in the Authorization header. A `user:pass@` URL
    // would otherwise show up in a reqwest error.
    let (url, from_url) = split_userinfo(&raw_url);
    let auth = match http_auth_from_init(params) {
        Ok(auth) => auth,
        Err(err) => match from_url {
            Some((user, pass)) => RpcAuth::Basic { user, pass },
            None => return Err(err),
        },
    };
    Ok(EndpointKind::Http { url, auth })
}

/// `scheme://user:pass@host` becomes `(scheme://host, Some((user, pass)))`.
fn split_userinfo(url: &str) -> (String, Option<(String, String)>) {
    let Some(scheme_at) = url.find("://") else {
        return (url.to_owned(), None);
    };
    let rest = &url[scheme_at + 3..];
    let Some((userinfo, host)) = rest.rsplit_once('@') else {
        return (url.to_owned(), None);
    };
    if host.is_empty() {
        return (url.to_owned(), None);
    }
    let bare = format!("{}{host}", &url[..scheme_at + 3]);
    let creds = match userinfo.split_once(':') {
        Some((user, pass)) if !user.is_empty() && !pass.is_empty() => {
            Some((user.to_owned(), pass.to_owned()))
        }
        _ => None,
    };
    (bare, creds)
}

fn redact_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(rel) = text[i..].find("://") {
            let start = i + rel;
            out.push_str(&text[i..start + 3]);
            let after = start + 3;
            if let Some(at) = text[after..].find('@') {
                let host = after + at + 1;
                let userinfo = &text[after..after + at];
                if userinfo.contains(':') {
                    i = host;
                    continue;
                }
            }
            i = start + 3;
            continue;
        }
        out.push_str(&text[i..]);
        break;
    }
    out
}

fn http_auth_from_init(params: &Value) -> Result<RpcAuth, String> {
    if let Some(token) = opt_str(params, "rpc-token") {
        return Ok(RpcAuth::Bearer(token));
    }
    if let Some(path) = opt_str(params, "rpc-token-file") {
        return Ok(RpcAuth::Bearer(read_token_file(&path)?));
    }
    if let Some(path) = opt_str(params, "rpc-cookie-file") {
        let raw = read_cookie_file(&path)?;
        let (user, pass) = split_cookie(&raw)?;
        return Ok(RpcAuth::Basic {
            user: user.to_owned(),
            pass: pass.to_owned(),
        });
    }
    let user = opt_str(params, "core-user");
    let pass = opt_str(params, "core-pass");
    match (user, pass) {
        (Some(user), Some(pass)) => Ok(RpcAuth::Basic { user, pass }),
        _ => Err(
            "rbitcoin plugin: TCP RPC needs rpc-token-file, rpc-cookie-file, or core-user and core-pass"
                .to_owned(),
        ),
    }
}

fn opt_str(params: &Value, key: &str) -> Option<String> {
    params
        .pointer(&format!("/options/{key}"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn read_token_file(path: &str) -> Result<String, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("rbitcoin credential file `{path}`: {err}"))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("rbitcoin credential file `{path}` is empty"));
    }
    Ok(trimmed.to_owned())
}

/// Drop a trailing newline only. A password may end in a space, and
/// `bitcoincore_rpc`'s cookie reader keeps that byte.
fn read_cookie_file(path: &str) -> Result<String, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("rbitcoin credential file `{path}`: {err}"))?;
    let raw = raw.trim_end_matches(['\r', '\n']);
    if raw.is_empty() {
        return Err(format!("rbitcoin credential file `{path}` is empty"));
    }
    Ok(raw.to_owned())
}

fn split_cookie(raw: &str) -> Result<(&str, &str), String> {
    let (user, pass) = raw
        .split_once(':')
        .ok_or("rbitcoin cookie file is not user:password")?;
    if user.is_empty() || pass.is_empty() {
        return Err("rbitcoin cookie file is not user:password".to_owned());
    }
    Ok((user, pass))
}

fn basic_auth(user: &str, pass: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
}

fn parse_rpc_result(method: &str, bytes: &[u8]) -> Result<Value, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|err| format!("rbitcoin `{method}` invalid JSON: {err}"))?;
    if let Some(error) = value.get("error").filter(|err| !err.is_null()) {
        let message = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("rbitcoin error");
        return Err(format!("rbitcoin `{method}`: {message}"));
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| format!("rbitcoin `{method}` returned no result"))
}

/// Lampo deserializes `loaded` and `mempoolminfee`. rbitcoin sends both;
/// fill `loaded` if a future build omits it, and fail closed without the fee.
fn normalize_mempool(value: Value) -> Result<Value, String> {
    let Some(mut obj) = value.as_object().cloned() else {
        return Err("rbitcoin getmempoolinfo was not an object".to_owned());
    };
    if !obj.contains_key("loaded") {
        obj.insert("loaded".to_owned(), json!(true));
    }
    if obj.get("mempoolminfee").and_then(|v| v.as_f64()).is_none() {
        return Err("rbitcoin getmempoolinfo missing mempoolminfee".to_owned());
    }
    Ok(Value::Object(obj))
}

fn http_body(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let header_end = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| bytes.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
        .ok_or("rbitcoin socket response had no HTTP header")?;
    let header = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| "rbitcoin socket response header was not UTF-8".to_owned())?;
    let status = header.lines().next().unwrap_or("");
    let body = bytes[header_end..].to_vec();
    // rbitcoin answers a JSON-RPC 1.0 failure with HTTP 500 and an `error`
    // object. Lampo's broadcast retry looks for `non-final` in that message.
    if !status.contains(" 200 ") && serde_json::from_slice::<Value>(&body).is_err() {
        return Err(format!("rbitcoin socket HTTP status `{status}`"));
    }
    Ok(body)
}

fn ensure_same_chain(socket_chain: &str, url_chain: &str) -> Result<(), String> {
    if socket_chain.is_empty() || url_chain.is_empty() {
        return Err("rbitcoin getblockchaininfo missing chain".to_owned());
    }
    if socket_chain != url_chain {
        return Err(format!(
            "rbitcoin rpc-socket is on `{socket_chain}` and core-url is on `{url_chain}`"
        ));
    }
    Ok(())
}

#[cfg(unix)]
async fn unix_post(path: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::UnixStream::connect(path)
        .await
        .map_err(|err| format!("rbitcoin socket `{path}`: {err}"))?;
    let header = format!(
        "POST / HTTP/1.1\r\nHost: rpc\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .await
        .map_err(|err| format!("rbitcoin socket `{path}` write: {err}"))?;
    stream
        .write_all(body)
        .await
        .map_err(|err| format!("rbitcoin socket `{path}` write: {err}"))?;
    stream.shutdown().await.ok();
    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .await
        .map_err(|err| format!("rbitcoin socket `{path}` read: {err}"))?;
    http_body(&buf)
}

#[cfg(not(unix))]
async fn unix_post(_path: &str, _body: &[u8]) -> Result<Vec<u8>, String> {
    Err("rbitcoin rpc-socket requires a unix host".to_owned())
}

fn rpc(
    node: Arc<RwLock<Option<Rbitcoin>>>,
    method: &'static str,
) -> impl Fn(
    Value,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>>
       + Send
       + Sync
       + Clone
       + 'static {
    move |params: Value| {
        let node = node.clone();
        let method = method.to_owned();
        Box::pin(async move {
            let args = match params {
                Value::Array(items) => items,
                Value::Object(map) if map.is_empty() => Vec::new(),
                other => {
                    return Err(format!(
                        "rbitcoin plugin: `{method}` params must be an array, got {other}"
                    ));
                }
            };
            let guard = node.read().await;
            let rpc = guard.as_ref().ok_or("rbitcoin plugin: init has not run")?;
            let value = rpc.call(&method, args).await?;
            if method == "getmempoolinfo" {
                normalize_mempool(value)
            } else {
                Ok(value)
            }
        })
    }
}

#[tokio::main]
async fn main() {
    let node = Arc::new(RwLock::new(None));
    let mut plugin = Plugin::new()
        .failure_mode(lampo_plugin_sdk::FailureMode::FailClosed)
        .important(true);
    for (name, description, usage) in CHAIN_METHODS {
        plugin = plugin.rpc_method(*name, *description, *usage, rpc(node.clone(), *name));
    }
    let node_for_init = node.clone();
    plugin
        .on_init(move |params| {
            let node_for_init = node_for_init.clone();
            async move {
                let rpc = Rbitcoin::from_init(&params)?;
                // Fail startup if the node is unreachable or the tip object
                // is not the shape Lampo decodes.
                let info = rpc.call("getblockchaininfo", Vec::new()).await?;
                if info.get("bestblockhash").and_then(|v| v.as_str()).is_none() {
                    return Err("rbitcoin getblockchaininfo missing bestblockhash".to_owned());
                }
                // The wallet dials `core-url` even when chain RPC uses the
                // socket. Same `chain` name, not the same tip: one side may
                // still be syncing.
                if matches!(&rpc.kind, EndpointKind::Unix { .. })
                    && opt_str(&params, "core-url").is_some()
                {
                    let mut http_params = params.clone();
                    if let Some(options) = http_params
                        .get_mut("options")
                        .and_then(|value| value.as_object_mut())
                    {
                        options.remove("rpc-socket");
                    }
                    let http = Rbitcoin::from_init(&http_params)?;
                    let url_info = http.call("getblockchaininfo", Vec::new()).await?;
                    let socket_chain = info.get("chain").and_then(|v| v.as_str()).unwrap_or("");
                    let url_chain = url_info.get("chain").and_then(|v| v.as_str()).unwrap_or("");
                    ensure_same_chain(socket_chain, url_chain)?;
                }
                *node_for_init.write().await = Some(rpc);
                Ok(())
            }
        })
        .start()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_with(options: Value) -> Value {
        json!({ "options": options })
    }

    #[test]
    fn socket_wins_over_url() {
        let params = init_with(json!({
            "rpc-socket": "/tmp/rbitcoin.sock",
            "core-url": "http://127.0.0.1:18443",
            "core-user": "u",
            "core-pass": "p",
        }));
        match endpoint_from_init(&params).unwrap() {
            EndpointKind::Unix { path } => assert_eq!(path, "/tmp/rbitcoin.sock"),
            EndpointKind::Http { .. } => panic!("socket should win"),
        }
    }

    #[test]
    fn bearer_file_and_cookie_and_userpass() {
        let dir = std::env::temp_dir().join(format!("lampo-rbitcoin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let token = dir.join("rpc.token");
        let cookie = dir.join("rpc.cookie");
        std::fs::write(&token, "abc123\n").unwrap();
        std::fs::write(&cookie, "lampo:secret").unwrap();

        let bearer = endpoint_from_init(&init_with(json!({
            "core-url": "http://127.0.0.1:18443",
            "rpc-token-file": token,
        })))
        .unwrap();
        match bearer {
            EndpointKind::Http {
                auth: RpcAuth::Bearer(token),
                ..
            } => {
                assert_eq!(token, "abc123");
            }
            other => panic!("expected bearer, got a different endpoint: {other:?}"),
        }

        let basic = endpoint_from_init(&init_with(json!({
            "core-url": "http://127.0.0.1:18443",
            "rpc-cookie-file": cookie,
        })))
        .unwrap();
        match basic {
            EndpointKind::Http {
                auth: RpcAuth::Basic { user, pass },
                ..
            } => {
                assert_eq!(user, "lampo");
                assert_eq!(pass, "secret");
            }
            other => panic!("expected cookie basic, got a different endpoint: {other:?}"),
        }

        let userpass = endpoint_from_init(&init_with(json!({
            "core-url": "http://127.0.0.1:18443",
            "core-user": "lampo",
            "core-pass": "lampo",
        })))
        .unwrap();
        assert!(matches!(
            userpass,
            EndpointKind::Http {
                auth: RpcAuth::Basic { .. },
                ..
            }
        ));

        assert!(endpoint_from_init(&init_with(json!({
            "core-url": "http://127.0.0.1:18443",
        })))
        .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cookie_without_colon_is_rejected() {
        assert!(split_cookie("noseparator").is_err());
        assert!(split_cookie(":secret").is_err());
        assert_eq!(split_cookie("user:p:a").unwrap(), ("user", "p:a"));
        assert_eq!(split_cookie("user:secret ").unwrap().1, "secret ");
    }

    #[test]
    fn url_userinfo_is_stripped_and_not_logged() {
        let params = init_with(json!({
            "core-url": "http://lampo:secret@127.0.0.1:18443",
        }));
        match endpoint_from_init(&params).unwrap() {
            EndpointKind::Http { url, auth } => {
                assert_eq!(url, "http://127.0.0.1:18443");
                match auth {
                    RpcAuth::Basic { user, pass } => {
                        assert_eq!(user, "lampo");
                        assert_eq!(pass, "secret");
                    }
                    RpcAuth::Bearer(_) => panic!("userinfo is basic auth"),
                }
            }
            EndpointKind::Unix { .. } => panic!("expected http"),
        }
        let leaked = "rbitcoin transport error: error sending request for url (http://lampo:secret@127.0.0.1:18443/)";
        assert!(!redact_userinfo(leaked).contains("secret"));
    }

    #[test]
    fn socket_and_url_must_be_the_same_chain() {
        assert!(ensure_same_chain("regtest", "regtest").is_ok());
        assert!(ensure_same_chain("regtest", "signet").is_err());
        assert!(ensure_same_chain("", "regtest").is_err());
    }

    #[test]
    fn rpc_error_and_mempool_shape() {
        let err = parse_rpc_result(
            "getblock",
            br#"{"error":{"message":"Block not found"},"result":null}"#,
        );
        assert!(err.unwrap_err().contains("Block not found"));

        let filled = normalize_mempool(json!({"mempoolminfee": 0.00001})).unwrap();
        assert_eq!(filled["loaded"], true);
        assert!(normalize_mempool(json!({"loaded": true})).is_err());
    }

    #[test]
    fn http_body_splits_status_and_payload() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(http_body(raw).unwrap(), b"{}");
        assert!(http_body(b"HTTP/1.1 401 Unauthorized\r\n\r\nno").is_err());
        let rpc_err = b"HTTP/1.1 500 Internal Server Error\r\n\r\n{\"error\":{\"message\":\"non-final\"},\"result\":null}";
        let body = http_body(rpc_err).unwrap();
        assert!(parse_rpc_result("sendrawtransaction", &body)
            .unwrap_err()
            .contains("non-final"));
    }
}

#[cfg(test)]
impl std::fmt::Debug for EndpointKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EndpointKind::Unix { path } => write!(f, "unix {path}"),
            EndpointKind::Http {
                url,
                auth: RpcAuth::Bearer(_),
            } => write!(f, "http {url} bearer"),
            EndpointKind::Http {
                url,
                auth: RpcAuth::Basic { .. },
            } => write!(f, "http {url} basic"),
        }
    }
}
