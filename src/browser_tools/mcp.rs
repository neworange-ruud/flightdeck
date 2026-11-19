//! `flightdeck browser-mcp`: the MCP server an agent runs to use the
//! session browser (`specs/SESSION_BROWSER_PRD.md` §6).
//!
//! MCP over stdio: one JSON-RPC 2.0 message per line in each direction. It
//! answers `initialize`, `ping`, `tools/list` and `tools/call`; every call is
//! sent on to the host's bridge ([`super::bridge`]) named in the ticket,
//! with the session it was launched for. Calls run on their own threads, so
//! a slow page does not hold up a quick `browser_tabs`.
//!
//! The ticket is read again for every call, so a host that restarted (a new
//! port and token) is found without restarting the agent.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::bridge::{read_ticket, BridgeRequest, CALL_TIMEOUT};
use super::tools::catalog;
use super::{BrowserReply, ReplyContent, MCP_SERVER_NAME};

/// The MCP revision answered when the client asks for none we know.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Revisions this server speaks (tools only, which none of them changed).
const KNOWN_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// What the agent reads about the server as a whole.
const INSTRUCTIONS: &str = "These tools drive the browser in the user's FlightDeck session, \
which the user can watch. Requests are made by the machine the session runs on, so \
localhost is that machine's (your dev server). Start with browser_tabs, or browser_open to \
open a tab; read a page with browser_snapshot and act on it by ref. The user must allow it \
per session; if a tool says they have not, or that they stopped you, ask them rather than \
retrying.";

/// Run the server on stdin and stdout until stdin closes. `ticket` and
/// `session` are what the agent was launched with.
pub fn run(ticket: PathBuf, session: String) -> i32 {
    let stdin = std::io::stdin();
    let out: Arc<Mutex<std::io::Stdout>> = Arc::new(Mutex::new(std::io::stdout()));
    let ticket = Arc::new(ticket);
    let session = Arc::new(session);
    let mut calls = Vec::new();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                send(
                    &out,
                    &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
                );
                continue;
            }
        };
        if message.get("method").and_then(Value::as_str) == Some("tools/call") {
            let out = Arc::clone(&out);
            let ticket = Arc::clone(&ticket);
            let session = Arc::clone(&session);
            calls.push(std::thread::spawn(move || {
                let id = message.get("id").cloned().unwrap_or(Value::Null);
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                let tool = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let reply = call(&ticket, &session, tool, args);
                send(
                    &out,
                    &json!({"jsonrpc":"2.0","id":id,"result":result_json(&reply)}),
                );
            }));
            continue;
        }
        if let Some(answer) = handle(&message) {
            send(&out, &answer);
        }
    }
    // Answer what is still running before going.
    for call in calls {
        let _ = call.join();
    }
    0
}

fn send(out: &Mutex<std::io::Stdout>, message: &Value) {
    let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

/// The answer to every message but `tools/call`, or `None` for a
/// notification.
pub fn handle(message: &Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // A notification (no id) is never answered.
    let id = id?;
    let result = match method {
        "initialize" => {
            let asked = message
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION);
            let version = if KNOWN_VERSIONS.contains(&asked) {
                asked
            } else {
                PROTOCOL_VERSION
            };
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": MCP_SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools_json() }),
        _ => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Method not found: {method}") },
            }))
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// The catalog as `tools/list` lists it.
pub fn tools_json() -> Value {
    Value::Array(
        catalog()
            .into_iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                })
            })
            .collect(),
    )
}

/// A reply as `tools/call`'s result.
pub fn result_json(reply: &BrowserReply) -> Value {
    let content: Vec<Value> = reply
        .content
        .iter()
        .map(|c| match c {
            ReplyContent::Text { text } => json!({ "type": "text", "text": text }),
            ReplyContent::Image { data, mime_type } => {
                json!({ "type": "image", "data": data, "mimeType": mime_type })
            }
        })
        .collect();
    json!({ "content": content, "isError": reply.is_error })
}

/// Send one call to the host named in the ticket at `ticket_path` and wait
/// for its answer.
pub fn call(ticket_path: &Path, session: &str, tool: String, args: Value) -> BrowserReply {
    let not_running = || {
        BrowserReply::error(
            "FlightDeck is not running here any more, so there is no browser to use. Use your \
             own headless browser instead.",
        )
    };
    let Ok(ticket) = read_ticket(ticket_path) else {
        return not_running();
    };
    let Ok(addr) = ticket.address.parse::<SocketAddr>() else {
        return not_running();
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(5)) else {
        return not_running();
    };
    let request = BridgeRequest {
        token: ticket.token,
        session: session.to_string(),
        tool,
        args,
    };
    let Ok(mut line) = serde_json::to_string(&request) else {
        return BrowserReply::error("FlightDeck could not send the request.");
    };
    line.push('\n');
    if stream.write_all(line.as_bytes()).is_err() {
        return not_running();
    }
    let _ = stream.set_read_timeout(Some(CALL_TIMEOUT + Duration::from_secs(5)));
    let mut answer = String::new();
    match BufReader::new(stream).read_line(&mut answer) {
        Ok(n) if n > 0 => serde_json::from_str(&answer)
            .unwrap_or_else(|_| BrowserReply::error("FlightDeck answered something unreadable.")),
        _ => BrowserReply::error(super::TIMED_OUT),
    }
}

/// Parse `browser-mcp`'s arguments: `--ticket <path> --session <id>`.
pub fn parse_args(args: &[String]) -> Result<(PathBuf, String), String> {
    let mut ticket = None;
    let mut session = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--ticket" => ticket = it.next().map(PathBuf::from),
            "--session" => session = it.next().cloned(),
            other => return Err(format!("browser-mcp: unknown argument {other}")),
        }
    }
    match (ticket, session) {
        (Some(t), Some(s)) => Ok((t, s)),
        _ => Err("usage: flightdeck browser-mcp --ticket <path> --session <id>".to_string()),
    }
}

/// `flightdeck browser-mcp …` (and the Desktop app's `browser-mcp`): the
/// process's exit code.
pub fn main(args: &[String]) -> i32 {
    match parse_args(args) {
        Ok((ticket, session)) => run(ticket, session),
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}
