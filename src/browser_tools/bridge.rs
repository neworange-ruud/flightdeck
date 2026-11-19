//! The host's end of the agent browser tools: where `flightdeck browser-mcp`
//! sends each call.
//!
//! A TCP listener on `127.0.0.1` (a random port), the same on every OS.
//! Its address and a random token are written to a **ticket** file,
//! `~/.flightdeck/browser/<host>.json`, which only this account can read
//! (0600 in a 0700 folder on Unix; the profile folder's own ACL on
//! Windows). A request without the token is refused, so another account on
//! the machine cannot drive anybody's browser. The agent is launched with
//! the ticket's path, never the token itself, so the token is in no
//! command line and no environment.
//!
//! One request per connection, each a line of JSON
//! (`{token, session, tool, args}`), answered with one line: a
//! [`BrowserReply`]. The listener never touches the host (which is not
//! `Send`): it hands each call to the host's turn loop
//! ([`BrowserBridge::take_calls`]) and waits for
//! [`BrowserBridge::answer`], at most [`CALL_TIMEOUT`].

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

use super::{BrowserCall, BrowserReply, TIMED_OUT};

/// How long a call waits for the window's answer. A page load is waited
/// for inside this (the window gives up on a load sooner), and Codex gives
/// a tool 60 s by default.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(45);

/// The longest request line read: an expression for `browser_eval` fits
/// many times over.
const MAX_REQUEST: u64 = 1024 * 1024;

/// What the ticket file holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    /// `127.0.0.1:<port>`.
    pub address: String,
    pub token: String,
}

/// One request from `flightdeck browser-mcp`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BridgeRequest {
    pub token: String,
    /// The session (tab id) the agent was launched for.
    pub session: String,
    pub tool: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// `~/.flightdeck/browser/<host>.json`, or `None` with no home folder.
pub fn ticket_path(host: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|h| !h.is_empty()))?;
    Some(
        PathBuf::from(home)
            .join(".flightdeck")
            .join("browser")
            .join(format!("{host}.json")),
    )
}

/// Calls waiting for their answer, by number.
type Waiting = Arc<Mutex<HashMap<u64, Sender<BrowserReply>>>>;

/// See the module docs. Dropping it stops the listener and removes the
/// ticket.
pub struct BrowserBridge {
    ticket_path: PathBuf,
    ticket: Ticket,
    addr: SocketAddr,
    calls: Receiver<BrowserCall>,
    waiting: Waiting,
    stop: Arc<AtomicBool>,
}

impl BrowserBridge {
    /// Listen, and write the ticket to `ticket_path`.
    pub fn start(ticket_path: PathBuf) -> std::io::Result<BrowserBridge> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let ticket = Ticket {
            address: addr.to_string(),
            token: new_token(),
        };
        write_ticket(&ticket_path, &ticket)?;
        let (tx, calls) = channel();
        let waiting: Waiting = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let next = Arc::new(AtomicU64::new(1));
        {
            let token = ticket.token.clone();
            let waiting = Arc::clone(&waiting);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("browser-bridge".to_string())
                .spawn(move || {
                    for stream in listener.incoming() {
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        let Ok(stream) = stream else { continue };
                        let token = token.clone();
                        let tx: Sender<BrowserCall> = tx.clone();
                        let waiting = Arc::clone(&waiting);
                        let next = Arc::clone(&next);
                        let _ = std::thread::Builder::new()
                            .name("browser-bridge-call".to_string())
                            .spawn(move || serve(stream, &token, &tx, &waiting, &next));
                    }
                })?;
        }
        Ok(BrowserBridge {
            ticket_path,
            ticket,
            addr,
            calls,
            waiting,
            stop,
        })
    }

    /// Where the ticket is: what `flightdeck browser-mcp --ticket` reads.
    pub fn ticket_path(&self) -> &Path {
        &self.ticket_path
    }

    /// The address it listens on.
    pub fn address(&self) -> SocketAddr {
        self.addr
    }

    /// Calls that arrived since the last look, oldest first.
    pub fn take_calls(&self) -> Vec<BrowserCall> {
        self.calls.try_iter().collect()
    }

    /// Whether anybody still waits for call `call`.
    pub fn is_waiting(&self, call: u64) -> bool {
        self.waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&call)
    }

    /// Answer call `call`. False when nobody waits for it any more (it
    /// timed out, or the agent went away).
    pub fn answer(&self, call: u64, reply: BrowserReply) -> bool {
        let sender = self
            .waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&call);
        sender.is_some_and(|tx| tx.send(reply).is_ok())
    }
}

impl Drop for BrowserBridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        // Only our own ticket: another host may have written one since.
        let ours = std::fs::read_to_string(&self.ticket_path)
            .ok()
            .and_then(|text| serde_json::from_str::<Ticket>(&text).ok())
            .is_some_and(|t| t == self.ticket);
        if ours {
            let _ = std::fs::remove_file(&self.ticket_path);
        }
    }
}

/// One connection: read the request, hand the call over, wait, answer.
fn serve(
    stream: TcpStream,
    token: &str,
    tx: &Sender<BrowserCall>,
    waiting: &Waiting,
    next: &AtomicU64,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut line = String::new();
    if BufReader::new(stream.take(MAX_REQUEST))
        .read_line(&mut line)
        .is_err()
    {
        return;
    }
    let reply = match serde_json::from_str::<BridgeRequest>(&line) {
        Err(_) => BrowserReply::error("FlightDeck could not read the request."),
        Ok(req) if !constant_time_eq(req.token.as_bytes(), token.as_bytes()) => {
            BrowserReply::error(
                "FlightDeck refused the request: its ticket is out of date. Restart the agent.",
            )
        }
        Ok(req) => {
            let call = next.fetch_add(1, Ordering::SeqCst);
            let (reply_tx, reply_rx) = channel();
            waiting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(call, reply_tx);
            let sent = tx
                .send(BrowserCall {
                    call,
                    session: req.session,
                    tool: req.tool,
                    args: req.args,
                })
                .is_ok();
            let reply = if sent {
                reply_rx.recv_timeout(CALL_TIMEOUT).ok()
            } else {
                None
            };
            waiting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&call);
            reply.unwrap_or_else(|| BrowserReply::error(TIMED_OUT))
        }
    };
    if let Ok(mut text) = serde_json::to_string(&reply) {
        text.push('\n');
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn new_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Write `ticket` to `path` so only this account can read it, replacing
/// what was there in one step.
fn write_ticket(path: &Path, ticket: &Ticket) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("the ticket has no folder"))?;
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("ticket"),
        std::process::id()
    ));
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(serde_json::to_string(ticket)?.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Read the ticket at `path`.
pub fn read_ticket(path: &Path) -> std::io::Result<Ticket> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(std::io::Error::other)
}
