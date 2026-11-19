//! Agent browser tools (`specs/SESSION_BROWSER_PRD.md` §6, epic
//! `remote-control-cefv.7`): the session's agent drives the session's
//! browser, and the developer watches it do so.
//!
//! ```text
//! agent (in the worktree)
//!   → flightdeck browser-mcp --ticket <file> --session <id>   stdio MCP, injected at launch
//!   → the host's browser bridge (127.0.0.1, a token in <file>)       [`bridge`]
//!   → the window holding the browser: the Desktop's own (local),
//!     or the Desktop window holding a worker's seat (protocol v13)
//!   → the browser tab, which acts and answers
//! ```
//!
//! - [`tools`]: the tool catalog the MCP server lists, and [`Tool`], one
//!   call parsed. Shared by the MCP server (which lists the tools) and the
//!   Desktop (which runs them), so a tool cannot exist on one side only.
//! - [`bridge`]: the host's end: a loopback listener whose address and
//!   token are in a ticket file only this account can read. It hands each
//!   call to the host's turn loop and waits for the answer.
//! - [`mcp`]: the MCP server the agent runs (`flightdeck browser-mcp`).
//! - [`launch`]: what the agent is launched with so it finds the server.
//!
//! Only a host with a browser somewhere enables the bridge: the Desktop app
//! for its local sessions and a worker for a Desktop that attaches. The TUI
//! never does, so its agents get no browser tools.

pub mod bridge;
pub mod launch;
pub mod mcp;
pub mod tools;

use serde::{Deserialize, Serialize};

pub use tools::Tool;

/// The MCP server's name, as the agent lists it (`mcp__flightdeck-browser__…`
/// in Claude Code).
pub const MCP_SERVER_NAME: &str = "flightdeck-browser";

/// One call from an agent, as the host hands it to the window with the
/// browser: which session's agent asked, which tool, and its arguments as
/// the agent sent them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserCall {
    /// The host's number for the call; the answer names it.
    pub call: u64,
    /// The session (tab id) whose agent asked.
    pub session: String,
    /// The tool's name, e.g. `browser_snapshot`.
    pub tool: String,
    /// Its arguments.
    #[serde(default)]
    pub args: serde_json::Value,
}

/// A tool's answer, as MCP's `tools/call` result carries it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserReply {
    pub content: Vec<ReplyContent>,
    /// The tool failed; `content` says why, in words for the agent.
    #[serde(default)]
    pub is_error: bool,
}

/// One piece of an answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplyContent {
    Text {
        text: String,
    },
    /// An image, base64-encoded.
    Image {
        data: String,
        mime_type: String,
    },
}

impl BrowserReply {
    /// A plain answer.
    pub fn text(text: impl Into<String>) -> BrowserReply {
        BrowserReply {
            content: vec![ReplyContent::Text { text: text.into() }],
            is_error: false,
        }
    }

    /// A failure, explained to the agent.
    pub fn error(text: impl Into<String>) -> BrowserReply {
        BrowserReply {
            content: vec![ReplyContent::Text { text: text.into() }],
            is_error: true,
        }
    }

    /// A PNG, base64-encoded, with a line saying what it shows.
    pub fn png(base64: String, caption: impl Into<String>) -> BrowserReply {
        BrowserReply {
            content: vec![
                ReplyContent::Text {
                    text: caption.into(),
                },
                ReplyContent::Image {
                    data: base64,
                    mime_type: "image/png".to_string(),
                },
            ],
            is_error: false,
        }
    }

    /// PNG bytes, encoded, with a line saying what they show.
    pub fn png_bytes(png: &[u8], caption: impl Into<String>) -> BrowserReply {
        use base64::Engine;
        BrowserReply::png(
            base64::engine::general_purpose::STANDARD.encode(png),
            caption,
        )
    }

    /// The text of every text part, joined: what a test or a log reads.
    pub fn joined_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| match c {
                ReplyContent::Text { text } => Some(text.as_str()),
                ReplyContent::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// What the agent is told when the user has not let it use the browser.
pub const NOT_ALLOWED: &str = "The user has not let you use the FlightDeck browser in this \
session. Ask them to open a browser in this session (New Browser) and turn on \"Let the agent \
use the browser\" in its toolbar, or use your own headless browser instead.";

/// What the agent is told when the user stopped it, or took the browser
/// over by using it.
pub const STOPPED: &str = "The user stopped you from using the FlightDeck browser (they \
pressed Stop or started using it themselves). Do not retry; ask the user before using the \
browser again.";

/// What an agent on a worker is told when no Desktop window is attached:
/// the browser lives in that window (B6).
pub const NO_DESKTOP: &str = "No FlightDeck Desktop window is attached to this worker, and the \
session browser lives in that window. Ask the user to open the worker in FlightDeck Desktop, \
or use your own headless browser instead.";

/// What an agent on a worker is told when the attached Desktop predates
/// agent browser tools.
pub const DESKTOP_TOO_OLD: &str = "The FlightDeck Desktop attached to this worker is too old \
for agent browser tools. Ask the user to update FlightDeck Desktop, or use your own headless \
browser instead.";

/// What the agent is told when the window did not answer in time.
pub const TIMED_OUT: &str = "The FlightDeck browser did not answer in time. The page may \
still be loading; try browser_snapshot, or try again.";

#[cfg(test)]
mod tests;
