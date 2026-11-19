//! What an agent is launched with so it finds the browser tools
//! (`specs/SESSION_BROWSER_PRD.md` §6, "Injection"): the same launch-scoped
//! idea as the status hooks ([`crate::agents::setup`]), nothing written to
//! the user's own configuration.
//!
//! - **Claude Code:** `--mcp-config <file>` naming a file in the status
//!   runtime folder. A server given this way needs no approval and adds to
//!   the user's own servers.
//! - **Codex:** `-c mcp_servers.flightdeck-browser.command=…` and `…args=…`,
//!   a session config layer like the hooks'.
//! - **OpenCode and Cursor:** not yet. OpenCode 2 runs one background
//!   service for every directory, so a server bound to one session would
//!   serve the others too; Cursor reads servers only from a file in the
//!   worktree and asks before starting one.
//!
//! A containerized agent gets nothing: the bridge listens on the host's
//! loopback and the ticket is in the host's home folder, neither of which
//! the container sees.

use std::path::{Path, PathBuf};

use crate::agents::setup::{status_backend, StatusBackend, STATUS_RUNTIME_DIR};
use crate::contracts::{AgentDef, FileSystem, Result};

use super::MCP_SERVER_NAME;

/// How to start `browser-mcp` on this host, and where its ticket is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserMcpLaunch {
    /// The executable: `flightdeck` on a worker, the Desktop app's own.
    pub command: String,
    /// Its arguments before ours: `["browser-mcp"]`.
    pub prefix: Vec<String>,
    /// The bridge's ticket ([`super::bridge`]).
    pub ticket: PathBuf,
}

impl BrowserMcpLaunch {
    /// This process's own executable, run as `<exe> browser-mcp`.
    pub fn this_executable(ticket: PathBuf) -> Option<BrowserMcpLaunch> {
        let exe = std::env::current_exe().ok()?;
        Some(BrowserMcpLaunch {
            command: exe.to_string_lossy().to_string(),
            prefix: vec!["browser-mcp".to_string()],
            ticket,
        })
    }

    /// The server's full argument list for `session`.
    pub fn args(&self, session: &str) -> Vec<String> {
        let mut args = self.prefix.clone();
        args.extend([
            "--ticket".to_string(),
            self.ticket.to_string_lossy().to_string(),
            "--session".to_string(),
            session.to_string(),
        ]);
        args
    }
}

/// The arguments that give `agent` the browser tools for `session`, writing
/// any file they name under `status_root`'s runtime folder. Empty for an
/// agent that cannot have them (see the module docs).
pub fn agent_args(
    fs: &dyn FileSystem,
    agent: &AgentDef,
    status_root: &Path,
    launch: &BrowserMcpLaunch,
    session: &str,
) -> Result<Vec<String>> {
    let server_args = launch.args(session);
    match status_backend(agent) {
        Some(StatusBackend::Claude) => {
            let runtime = status_root.join(STATUS_RUNTIME_DIR);
            fs.create_dir_all(&runtime)?;
            let file = runtime.join("claude-browser-mcp.json");
            let config = serde_json::json!({
                "mcpServers": {
                    MCP_SERVER_NAME: {
                        "type": "stdio",
                        "command": launch.command,
                        "args": server_args,
                    }
                }
            });
            fs.write(&file, &config.to_string())?;
            Ok(vec![
                "--mcp-config".to_string(),
                file.to_string_lossy().to_string(),
            ])
        }
        Some(StatusBackend::Codex) => {
            let args =
                toml::Value::Array(server_args.into_iter().map(toml::Value::String).collect());
            Ok(vec![
                "-c".to_string(),
                format!(
                    "mcp_servers.{MCP_SERVER_NAME}.command={}",
                    toml::Value::String(launch.command.clone())
                ),
                "-c".to_string(),
                format!("mcp_servers.{MCP_SERVER_NAME}.args={args}"),
            ])
        }
        Some(StatusBackend::OpenCode) | Some(StatusBackend::Cursor) | None => Ok(Vec::new()),
    }
}
