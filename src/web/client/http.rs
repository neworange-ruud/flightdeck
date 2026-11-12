//! The hand-rolled HTTP/1.1 a file transfer speaks to a worker: what
//! [`super::upload_http`] (`PUT /upload/{ticket}`) and
//! [`super::download_http`] (`GET /download/{ticket}`) share.
//!
//! The same as [`super::exchange`] — plain `http://` (the Desktop dials a
//! worker's protocol routes in the clear, inside the tailnet, exactly as it
//! dials `ws://`), the link's own `Cookie: flightdeck_web=…` and
//! `User-Agent`, `Connection: close` — with two things a transfer needs on
//! top: the response head read on its own (a download's body is streamed to
//! disk after it, never held whole), and a refusal's JSON
//! (`{ok: false, reason, message}`) read into its parts.

use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

use crate::web::server::COOKIE_NAME;

use super::exchange::{parse_response, AccessToken, Response, REQUEST_TIMEOUT};

/// The largest response head read before the worker counts as broken.
const MAX_HEAD: usize = 16 * 1024;

/// Where and as whom to transfer: the link's own address, token and agent.
#[derive(Clone, Debug)]
pub struct HttpAccess {
    /// `host:port` of the worker's web interface.
    pub address: String,
    /// The access token (the `flightdeck_web` cookie).
    pub token: AccessToken,
    /// Sent as `User-Agent`, as the link sends it.
    pub user_agent: String,
}

/// Dial `address`, giving up after [`REQUEST_TIMEOUT`].
pub(super) async fn connect(address: &str) -> Result<TcpStream, String> {
    match tokio::time::timeout(REQUEST_TIMEOUT, TcpStream::connect(address)).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("timed out connecting".to_string()),
    }
}

/// A request head for `method path` as `access`: host, `Connection: close`,
/// the cookie and the agent, then `extra` headers, then the blank line.
pub(super) fn request_head(
    method: &str,
    path: &str,
    access: &HttpAccess,
    extra: &[(&str, String)],
) -> String {
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\
         Cookie: {COOKIE_NAME}={token}\r\nUser-Agent: {agent}\r\n",
        address = access.address,
        token = access.token.reveal(),
        agent = access.user_agent.replace(['\r', '\n'], " "),
    );
    for (name, value) in extra {
        head.push_str(&format!("{name}: {}\r\n", value.replace(['\r', '\n'], " ")));
    }
    head.push_str("\r\n");
    head
}

/// A response's status line and headers.
pub(super) struct Head {
    /// The status code.
    pub(super) status: u16,
    /// The headers, as sent.
    headers: Vec<(String, String)>,
}

impl Head {
    /// The first value of header `name` (any case).
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// `Content-Length`, when sent and a number.
    pub(super) fn content_length(&self) -> Option<u64> {
        self.header("content-length")?.trim().parse().ok()
    }
}

impl From<Response> for Head {
    fn from(response: Response) -> Head {
        Head {
            status: response.status,
            headers: response.headers,
        }
    }
}

/// Read a response head from `stream`, waiting at most `wait` for it. Returns
/// it and whatever body bytes came in the same reads.
pub(super) async fn read_head(
    stream: &mut TcpStream,
    wait: Duration,
) -> Result<(Head, Vec<u8>), String> {
    let read = async {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                let rest = raw.split_off(at + 4);
                let head = parse_response(&raw).map_err(|e| e.sentence())?;
                return Ok((Head::from(head), rest));
            }
            if raw.len() > MAX_HEAD {
                return Err("the worker sent an oversized answer".to_string());
            }
            let n = stream.read(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Err(if raw.is_empty() {
                    "the worker closed without answering".to_string()
                } else {
                    "the worker's answer was cut off".to_string()
                });
            }
            raw.extend_from_slice(&buf[..n]);
        }
    };
    match tokio::time::timeout(wait, read).await {
        Ok(result) => result,
        Err(_) => Err("the worker did not answer".to_string()),
    }
}

/// A worker's refusal, from its status and JSON body: its `reason` code
/// (`unexpected_answer` when it gave none) and its sentence, when it gave one.
pub(super) fn refusal_parts(body: &str) -> (String, Option<String>) {
    let body: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    (
        body["reason"]
            .as_str()
            .unwrap_or("unexpected_answer")
            .to_string(),
        body["message"].as_str().map(str::to_string),
    )
}

/// The words for a refusal the worker gave no sentence for, when its reason
/// is one any route gives (the cookie's).
pub(super) fn access_sentence(reason: &str) -> Option<&'static str> {
    match reason {
        "token_revoked" => Some("The worker withdrew this app's access."),
        "unknown_token" => Some("The worker does not know this app any more."),
        "rate_limited" => Some("Too many attempts; wait a moment."),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_head_carries_the_links_cookie_and_agent_and_no_line_breaks() {
        let access = HttpAccess {
            address: "100.64.0.2:7420".to_string(),
            token: AccessToken::new("s3cret"),
            user_agent: "FlightDeck Desktop/1.0\r\nX-Evil: 1".to_string(),
        };
        let head = request_head(
            "GET",
            "/download/t",
            &access,
            &[("Accept", "*/*".to_string())],
        );
        assert!(head.starts_with("GET /download/t HTTP/1.1\r\nHost: 100.64.0.2:7420\r\n"));
        assert!(head.contains("Cookie: flightdeck_web=s3cret\r\n"));
        assert!(head.contains("User-Agent: FlightDeck Desktop/1.0  X-Evil: 1\r\n"));
        assert!(head.ends_with("Accept: */*\r\n\r\n"));
    }

    #[test]
    fn a_refusal_without_json_is_an_unexpected_answer() {
        assert_eq!(
            refusal_parts("<html>"),
            ("unexpected_answer".to_string(), None)
        );
        assert_eq!(
            refusal_parts(r#"{"ok":false,"reason":"file_changed","message":"It changed."}"#),
            ("file_changed".to_string(), Some("It changed.".to_string()))
        );
    }
}
