//! `GET /tunnel`: one TCP connection made by a worker on this machine's
//! behalf, for the session browser (v11, `specs/SESSION_BROWSER_PRD.md` B4;
//! the worker's side is `web::server::tunnel_route`).
//!
//! The same hand-rolled HTTP/1.1 as a file transfer ([`super::http`]) — plain
//! `http://` inside the tailnet, the link's own cookie and agent — except that
//! the request asks to upgrade, carries the key the worker granted
//! ([`super::RemoteClient::open_browser_tunnel`]) and the `host:port` to
//! reach, and a `101` hands back the socket itself: from then on it is the
//! target's bytes, both ways.

use std::time::Duration;

use tokio::net::TcpStream;

use super::http::{access_sentence, connect, read_head, refusal_parts, HttpAccess};
use crate::web::server::COOKIE_NAME;
use crate::web::tunnel::{TUNNEL_KEY_HEADER, TUNNEL_PATH, TUNNEL_TARGET_HEADER, TUNNEL_UPGRADE};

/// How long the worker may take to answer: it reaches the target first
/// (its own limit is ten seconds), so a little longer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(15);

/// Why a tunnel was not opened: the worker's reason code and its sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TunnelRefusal {
    /// `invalid_tunnel`, `refused`, `unresolved`, `timed_out`, …; `unreachable`
    /// when the worker itself could not be reached.
    pub reason: String,
    /// In words for a person (the browser's error page).
    pub message: String,
}

/// An open tunnel: the socket, now the target's, and any of the target's
/// bytes that arrived with the worker's answer.
pub struct Tunnel {
    pub stream: TcpStream,
    pub early: Vec<u8>,
}

/// Open one tunnel to `target` (`host:port`, IPv6 bracketed) through the
/// worker at `access`, with `key`.
pub async fn open(access: &HttpAccess, key: &str, target: &str) -> Result<Tunnel, TunnelRefusal> {
    let unreachable = |message: String| TunnelRefusal {
        reason: "unreachable".to_string(),
        message,
    };
    let mut stream = connect(&access.address)
        .await
        .map_err(|e| unreachable(format!("Could not reach the worker: {e}")))?;
    let head = request(access, key, target);
    tokio::io::AsyncWriteExt::write_all(&mut stream, head.as_bytes())
        .await
        .map_err(|e| unreachable(format!("Could not reach the worker: {e}")))?;
    let (answer, early) = read_head(&mut stream, ANSWER_TIMEOUT)
        .await
        .map_err(unreachable)?;
    if answer.status == 101 {
        return Ok(Tunnel { stream, early });
    }
    // A refusal is a small JSON body after the head.
    let mut body = early;
    let wanted = answer.content_length().unwrap_or(0).min(16 * 1024) as usize;
    while body.len() < wanted {
        let mut buf = [0u8; 4096];
        match tokio::time::timeout(
            ANSWER_TIMEOUT,
            tokio::io::AsyncReadExt::read(&mut stream, &mut buf),
        )
        .await
        {
            Ok(Ok(n)) if n > 0 => body.extend_from_slice(&buf[..n]),
            _ => break,
        }
    }
    let (reason, message) = refusal_parts(&String::from_utf8_lossy(&body));
    let message = message
        .or_else(|| access_sentence(&reason).map(str::to_string))
        .unwrap_or_else(|| format!("The worker refused the connection ({}).", answer.status));
    Err(TunnelRefusal { reason, message })
}

/// The request head: the upgrade, the link's cookie and agent, the key and
/// the target. Line breaks in any value are flattened, as in every head.
fn request(access: &HttpAccess, key: &str, target: &str) -> String {
    let flat = |v: &str| v.replace(['\r', '\n'], " ");
    format!(
        "GET {TUNNEL_PATH} HTTP/1.1\r\nHost: {address}\r\nConnection: Upgrade\r\n\
         Upgrade: {TUNNEL_UPGRADE}\r\nCookie: {COOKIE_NAME}={token}\r\nUser-Agent: {agent}\r\n\
         {TUNNEL_KEY_HEADER}: {key}\r\n{TUNNEL_TARGET_HEADER}: {target}\r\n\r\n",
        address = access.address,
        token = access.token.reveal(),
        agent = flat(&access.user_agent),
        key = flat(key),
        target = flat(target),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::client::exchange::AccessToken;

    #[test]
    fn the_request_upgrades_with_the_cookie_key_and_target_and_no_line_breaks() {
        let access = HttpAccess {
            address: "box.tail.ts.net:7420".to_string(),
            token: AccessToken::new("secret".to_string()),
            user_agent: "FlightDeck Desktop/1.0\r\nX-Evil: 1".to_string(),
        };
        let head = request(&access, "key\r\nX: y", "localhost:3000");
        assert!(head.starts_with("GET /tunnel HTTP/1.1\r\n"), "{head}");
        assert!(head.contains("\r\nConnection: Upgrade\r\n"));
        assert!(head.contains("\r\nUpgrade: flightdeck-tunnel\r\n"));
        assert!(head.contains(&format!("\r\nCookie: {COOKIE_NAME}=secret\r\n")));
        assert!(head.contains("\r\nx-flightdeck-target: localhost:3000\r\n"));
        assert!(head.ends_with("\r\n\r\n"));
        assert!(!head.contains("\r\nX-Evil"), "{head}");
        assert!(!head.contains("\r\nX: y"), "{head}");
    }
}
