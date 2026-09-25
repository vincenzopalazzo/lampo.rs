use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use lampo_common::async_trait;
use lampo_common::error;
use lampo_common::handler::ExternalHandler;
use lampo_common::json;
use lampo_common::jsonrpc::Request;

/// Posts JSON-RPC methods to the local httpd as `POST /{method}`.
///
/// This used to go through libcurl. A parallel test suite calls
/// `curl_global_init` from many threads, and libcurl then rejects a valid
/// URL with error 43. A plain TCP POST has no process-global init.
pub struct HttpdHandler {
    host: String,
    port: u16,
}

impl HttpdHandler {
    /// `host` is `http://127.0.0.1:port` or `127.0.0.1:port`.
    pub fn new(host: String) -> error::Result<Self> {
        let (host, port) = parse_host_port(&host)?;
        Ok(Self { host, port })
    }
}

fn parse_host_port(raw: &str) -> error::Result<(String, u16)> {
    let trimmed = raw.trim();
    let without_scheme = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .unwrap_or(trimmed);
    let host_port = without_scheme.split('/').next().unwrap_or("");
    let (host, port) = host_port
        .rsplit_once(':')
        .ok_or_else(|| error::anyhow!("httpd client URL `{raw}` needs a host and a port"))?;
    let host = host.trim_matches(|ch| ch == '[' || ch == ']');
    if host.is_empty() {
        error::bail!("httpd client URL `{raw}` has an empty host");
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| error::anyhow!("httpd client URL `{raw}` has no port"))?;
    Ok((host.to_owned(), port))
}

fn post_method(host: &str, port: u16, method: &str, body: &[u8]) -> error::Result<Vec<u8>> {
    if method.is_empty() || method.contains(['/', ' ', '\n', '\r', '?']) {
        error::bail!("refusing to post RPC method `{method}`");
    }
    // A parallel suite can hit EAGAIN on connect. Retry that; a refused
    // connection is a real failure and should not be retried forever.
    let mut stream = None;
    let mut last_err = None;
    for attempt in 1..=5 {
        match TcpStream::connect((host, port)) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                last_err = Some(err);
                std::thread::sleep(Duration::from_millis(50 * attempt));
            }
            Err(err) => return Err(err.into()),
        }
    }
    let mut stream = stream.ok_or_else(|| {
        error::anyhow!(
            "connecting to httpd `{host}:{port}`: {}",
            last_err
                .map(|err| err.to_string())
                .unwrap_or_else(|| "no connection".to_owned())
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let request = format!(
        "POST /{method} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    stream.write_all(body)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| error::anyhow!("httpd response for `{method}` had no header"))?;
    let header = String::from_utf8_lossy(&response[..header_end]);
    let status = header
        .lines()
        .next()
        .ok_or_else(|| error::anyhow!("httpd response for `{method}` had no status"))?;
    if !status.contains(" 200 ") && !status.contains(" 201 ") {
        error::bail!("httpd `{method}` returned `{status}`");
    }
    Ok(decode_body(&header, &response[header_end + 4..])?)
}

fn decode_body(header: &str, body: &[u8]) -> error::Result<Vec<u8>> {
    let chunked = header
        .lines()
        .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"));
    if !chunked {
        return Ok(body.to_vec());
    }
    let mut decoded = Vec::new();
    let mut rest = body;
    loop {
        let line_end = rest
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| error::anyhow!("chunked httpd body ended early"))?;
        let size = std::str::from_utf8(&rest[..line_end])?.trim();
        let size = usize::from_str_radix(size, 16)
            .map_err(|_| error::anyhow!("bad chunk size `{size}`"))?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            break;
        }
        if rest.len() < size + 2 {
            error::bail!("chunked httpd body is short");
        }
        decoded.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
    Ok(decoded)
}

#[async_trait]
impl ExternalHandler for HttpdHandler {
    async fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>> {
        let method = req.method.clone();
        let body = json::to_vec(&req.params)?;
        let host = self.host.clone();
        let port = self.port;
        let bytes = tokio::task::spawn_blocking(move || post_method(&host, port, &method, &body))
            .await
            .map_err(|err| error::anyhow!("httpd client task failed: {err}"))??;
        let response: json::Value = json::from_slice(&bytes)?;
        Ok(Some(response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_scheme_and_a_bare_host() {
        assert_eq!(
            parse_host_port("http://127.0.0.1:7979").unwrap(),
            ("127.0.0.1".to_owned(), 7979)
        );
        assert_eq!(
            parse_host_port("127.0.0.1:9").unwrap(),
            ("127.0.0.1".to_owned(), 9)
        );
    }

    #[test]
    fn decodes_a_chunked_body() {
        let body = b"5\r\nhello\r\n0\r\n\r\n";
        let decoded = decode_body("Transfer-Encoding: chunked", body).unwrap();
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn rejects_a_method_that_is_not_a_path_segment() {
        let err = post_method("127.0.0.1", 9, "get info", b"{}").unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
    }
}
