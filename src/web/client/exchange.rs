//! The two HTTP requests a native client makes before it can open `/ws`
//! (`specs/WEB_INTERFACE.md` Q4, `specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.2).
//!
//! `POST /auth/exchange` trades the controlled instance's 4-digit code for the
//! `flightdeck_web` cookie; `GET /auth/session` asks whether a stored token is
//! still good. Hand-rolled HTTP/1.1 over a `TcpStream`, exactly as
//! `tests/web_server.rs` speaks it: two requests do not justify an HTTP client
//! crate, and `Connection: close` makes reading to EOF the whole response.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::web::server::{AUTH_EXCHANGE_PATH, AUTH_SESSION_PATH, COOKIE_NAME};

/// How long either request may take, connect included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The largest response this reads. Both bodies are a few hundred bytes.
const MAX_RESPONSE: usize = 64 * 1024;

/// The bearer secret the host minted for this client: the `flightdeck_web`
/// cookie's value. Never printed by `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken(String);

impl AccessToken {
    /// Wrap a stored secret.
    pub fn new(secret: impl Into<String>) -> AccessToken {
        AccessToken(secret.into())
    }

    /// The secret itself, for the cookie header and the token store only.
    pub fn reveal(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken(…)")
    }
}

/// Why a request did not produce what was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExchangeError {
    /// Nothing answered at the address, or it answered too slowly.
    Unreachable(String),
    /// The host answered and refused. `reason` is its own spelling
    /// (`wrong_code`, `code_expired`, `rate_limited`, `token_revoked`, …).
    Refused {
        reason: String,
        attempts_remaining: Option<u32>,
        retry_after_ms: Option<u64>,
    },
    /// Something answered that does not speak this protocol.
    Protocol(String),
}

impl ExchangeError {
    /// One sentence for the connect overlay.
    pub fn sentence(&self) -> String {
        match self {
            ExchangeError::Unreachable(detail) => format!(
                "Nothing answered at that address ({detail}). Check that the web interface is \
                 running there in network mode."
            ),
            ExchangeError::Refused {
                reason,
                attempts_remaining,
                retry_after_ms,
            } => {
                let base = match reason.as_str() {
                    "wrong_code" => "That code is not the one the host is showing.",
                    "code_expired" => {
                        "That code has expired. Show the access overlay again for a new one."
                    }
                    "code_already_used" => {
                        "That code was already used. Show the access overlay again for a new one."
                    }
                    "no_code_outstanding" => {
                        "The host is not showing a code. Open its web access overlay first."
                    }
                    "rate_limited" => "Too many attempts from this address.",
                    "token_revoked" => {
                        "The host withdrew this app's access. Pair again with a new code."
                    }
                    "unknown_token" => {
                        "The host does not know this app. Pair again with a new code."
                    }
                    _ => "The host refused.",
                };
                match (retry_after_ms, attempts_remaining) {
                    (Some(ms), _) => format!("{base} Try again in {}s.", ms.div_ceil(1000)),
                    (None, Some(n)) if reason == "wrong_code" => {
                        format!("{base} {n} attempts left.")
                    }
                    _ => base.to_string(),
                }
            }
            ExchangeError::Protocol(detail) => {
                format!("That address did not answer like FlightDeck ({detail}).")
            }
        }
    }
}

/// What a probe of a stored token found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    /// The token still works.
    Valid,
    /// It does not; the host's reason.
    Refused(String),
}

/// Trade `code` for a token. `label` is how the host lists this client in its
/// access overlay (`FlightDeck Desktop/<version>`).
pub async fn exchange_code(
    address: &str,
    code: &str,
    label: &str,
) -> Result<AccessToken, ExchangeError> {
    let body = serde_json::json!({ "code": code, "label": label }).to_string();
    let response = request(address, "POST", AUTH_EXCHANGE_PATH, &[], Some(&body)).await?;
    if response.status == 200 {
        return response
            .cookie(COOKIE_NAME)
            .map(AccessToken)
            .ok_or_else(|| ExchangeError::Protocol("no access cookie in the answer".to_string()));
    }
    Err(refusal(&response))
}

/// Ask whether `token` still works, without opening a socket.
pub async fn probe_session(address: &str, token: &AccessToken) -> Result<Probe, ExchangeError> {
    let cookie = format!("{COOKIE_NAME}={}", token.reveal());
    let response = request(
        address,
        "GET",
        AUTH_SESSION_PATH,
        &[("Cookie", &cookie)],
        None,
    )
    .await?;
    if response.status == 200 {
        return Ok(Probe::Valid);
    }
    match refusal(&response) {
        ExchangeError::Refused { reason, .. } => Ok(Probe::Refused(reason)),
        other => Err(other),
    }
}

fn refusal(response: &Response) -> ExchangeError {
    let body: serde_json::Value =
        serde_json::from_str(&response.body).unwrap_or(serde_json::Value::Null);
    match body["reason"].as_str() {
        Some(reason) => ExchangeError::Refused {
            reason: reason.to_string(),
            attempts_remaining: body["attempts_remaining"].as_u64().map(|n| n as u32),
            retry_after_ms: body["retry_after_ms"].as_u64(),
        },
        None => ExchangeError::Protocol(format!("HTTP {}", response.status)),
    }
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Response {
    /// The value of the `name=value` pair in `Set-Cookie`, attributes dropped.
    fn cookie(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("set-cookie"))
            .filter_map(|(_, value)| value.split(';').next())
            .filter_map(|pair| pair.trim().split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.to_string())
    }
}

async fn request(
    address: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> Result<Response, ExchangeError> {
    match tokio::time::timeout(
        REQUEST_TIMEOUT,
        request_inner(address, method, path, headers, body),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(ExchangeError::Unreachable("timed out".to_string())),
    }
}

async fn request_inner(
    address: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> Result<Response, ExchangeError> {
    let mut stream = TcpStream::connect(address)
        .await
        .map_err(|e| ExchangeError::Unreachable(e.to_string()))?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    let io = |e: std::io::Error| ExchangeError::Unreachable(e.to_string());
    stream.write_all(head.as_bytes()).await.map_err(io)?;
    if let Some(body) = body {
        stream.write_all(body.as_bytes()).await.map_err(io)?;
    }
    let mut raw = Vec::new();
    (&mut stream)
        .take(MAX_RESPONSE as u64)
        .read_to_end(&mut raw)
        .await
        .map_err(io)?;
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Result<Response, ExchangeError> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| ExchangeError::Protocol("not an HTTP response".to_string()))?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .filter(|line| line.starts_with("HTTP/1."))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| ExchangeError::Protocol("not an HTTP response".to_string()))?;
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect();
    Ok(Response {
        status,
        headers,
        body: body.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cookie_value_is_read_without_its_attributes() {
        let raw = b"HTTP/1.1 200 OK\r\nset-cookie: flightdeck_web=s3cret; Path=/; HttpOnly\r\n\r\n{\"ok\":true}";
        let response = parse_response(raw).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.cookie(COOKIE_NAME).as_deref(), Some("s3cret"));
    }

    #[test]
    fn a_refusal_keeps_the_hosts_reason_and_numbers() {
        let raw = b"HTTP/1.1 401 Unauthorized\r\n\r\n{\"ok\":false,\"reason\":\"wrong_code\",\"attempts_remaining\":2}";
        let error = refusal(&parse_response(raw).unwrap());
        assert_eq!(
            error,
            ExchangeError::Refused {
                reason: "wrong_code".to_string(),
                attempts_remaining: Some(2),
                retry_after_ms: None,
            }
        );
        assert!(error.sentence().contains("2 attempts left"));
    }

    #[test]
    fn something_that_is_not_http_is_a_protocol_error() {
        assert!(matches!(
            parse_response(b"SSH-2.0-OpenSSH_9.6\r\n"),
            Err(ExchangeError::Protocol(_))
        ));
    }

    #[test]
    fn the_token_never_prints() {
        assert_eq!(
            format!("{:?}", AccessToken::new("s3cret")),
            "AccessToken(…)"
        );
    }
}
