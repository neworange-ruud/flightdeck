//! The host's share of the agent browser tools
//! (`specs/SESSION_BROWSER_PRD.md` §6, [`crate::browser_tools`]).
//!
//! A host with a browser somewhere enables them
//! ([`AppHost::enable_browser_tools`]): the Desktop app for its local
//! sessions, a worker for the Desktop that attaches. It then owns the
//! bridge, launches every agent with the MCP server, and each turn routes
//! the calls that arrived:
//!
//! - **Desktop:** to the front-end, which owns the browser views
//!   ([`AppHost::take_browser_calls`], answered with
//!   [`AppHost::answer_browser_call`]).
//! - **Worker:** to the window holding its seat, as a
//!   [`ServerMsg::BrowserToolCall`] (v13), answered with
//!   [`crate::web::protocol::worker_command::BROWSER_TOOL_RESULT`]. With no
//!   window, or one too old, the agent is told so at once. A call whose
//!   window went away is answered for it.

use std::collections::HashMap;

use crate::browser_tools::bridge::{ticket_path, BrowserBridge};
use crate::browser_tools::launch::BrowserMcpLaunch;
use crate::browser_tools::{BrowserCall, BrowserReply, DESKTOP_TOO_OLD, NO_DESKTOP};
use crate::contracts::TabId;
use crate::web::protocol::{
    BrowserToolCall, ConnectionKind, ServerMsg, ViewerId, AGENT_BROWSER_MIN_VERSION,
};
use crate::web::server::WebOutbound;

use super::AppHost;

/// What a host with browser tools keeps.
pub(super) struct BrowserTools {
    bridge: BrowserBridge,
    launch: BrowserMcpLaunch,
    /// Calls for the front-end (a Desktop host), until taken.
    outbox: Vec<BrowserCall>,
    /// Calls sent to a worker's window, and to which.
    sent: HashMap<u64, ViewerId>,
}

/// What the agent hears when the window it was sent to went away.
const WINDOW_GONE: &str = "The FlightDeck Desktop window holding the browser went away \
before answering. Ask the user to reopen it, or use your own headless browser instead.";

impl AppHost<'_> {
    /// Give this host's agents the session browser's tools: start the
    /// bridge and launch every agent from now on with the MCP server. A
    /// worker and the Desktop app call this; the TUI never does. Calling it
    /// again changes nothing.
    pub fn enable_browser_tools(&mut self) -> Result<(), String> {
        if self.browser_tools.is_some() {
            return Ok(());
        }
        let name = if self.worker.is_some() {
            "worker"
        } else {
            "desktop"
        };
        let ticket = ticket_path(name).ok_or("There is no home folder to keep the ticket in.")?;
        self.enable_browser_tools_at(ticket)
    }

    /// [`AppHost::enable_browser_tools`] with the ticket at `ticket`: tests
    /// keep it out of the real home folder.
    pub fn enable_browser_tools_at(&mut self, ticket: std::path::PathBuf) -> Result<(), String> {
        if self.browser_tools.is_some() {
            return Ok(());
        }
        let launch = BrowserMcpLaunch::this_executable(ticket.clone())
            .ok_or("FlightDeck could not find its own executable.")?;
        let bridge = BrowserBridge::start(ticket).map_err(|e| e.to_string())?;
        // Agents launched from here on, resumed ones included, get them.
        for project in &mut self.workspace.projects {
            project.state.browser_mcp = Some(launch.clone());
        }
        self.browser_tools = Some(BrowserTools {
            bridge,
            launch,
            outbox: Vec::new(),
            sent: HashMap::new(),
        });
        Ok(())
    }

    /// One turn's routing (see the module docs). True when calls arrived
    /// for the front-end.
    pub(super) fn pump_browser_tools(&mut self) -> bool {
        let Some(tools) = self.browser_tools.as_mut() else {
            return false;
        };
        // Every project, including one opened since, launches its agents
        // with the server.
        for project in &mut self.workspace.projects {
            if project.state.browser_mcp.is_none() {
                project.state.browser_mcp = Some(tools.launch.clone());
            }
        }
        let calls = tools.bridge.take_calls();
        if self.worker.is_none() {
            let arrived = !calls.is_empty();
            tools.outbox.extend(calls);
            return arrived;
        }
        let windows: Vec<(ViewerId, u16)> = self
            .web_surface
            .handle
            .as_ref()
            .map(|h| {
                h.connections()
                    .into_iter()
                    // A browser tab or `flightdeck connect` may hold the
                    // seat too; neither has a session browser.
                    .filter(|c| c.kind == ConnectionKind::Window && c.desktop)
                    .map(|c| (c.viewer_id, c.protocol_version))
                    .collect()
            })
            .unwrap_or_default();
        // A call whose window went, or that nobody waits for any more.
        let gone: Vec<u64> = tools
            .sent
            .iter()
            .filter(|(call, viewer)| {
                !tools.bridge.is_waiting(**call) || !windows.iter().any(|(v, _)| v == *viewer)
            })
            .map(|(call, _)| *call)
            .collect();
        for call in gone {
            tools.sent.remove(&call);
            tools.bridge.answer(call, BrowserReply::error(WINDOW_GONE));
        }
        for call in calls {
            match windows.first() {
                None => {
                    tools
                        .bridge
                        .answer(call.call, BrowserReply::error(NO_DESKTOP));
                }
                Some((_, version)) if *version < AGENT_BROWSER_MIN_VERSION => {
                    tools
                        .bridge
                        .answer(call.call, BrowserReply::error(DESKTOP_TOO_OLD));
                }
                Some((viewer, _)) => {
                    if let Some(handle) = self.web_surface.handle.as_ref() {
                        handle.send(WebOutbound::Viewer {
                            viewer_id: viewer.clone(),
                            msg: ServerMsg::BrowserToolCall(BrowserToolCall {
                                call: call.call,
                                session_id: TabId(call.session),
                                tool: call.tool,
                                args: call.args,
                            }),
                        });
                        tools.sent.insert(call.call, viewer.clone());
                    }
                }
            }
        }
        false
    }

    /// The agents' calls for the front-end's browsers since the last look
    /// (a Desktop host), oldest first.
    pub fn take_browser_calls(&mut self) -> Vec<BrowserCall> {
        self.browser_tools
            .as_mut()
            .map(|t| std::mem::take(&mut t.outbox))
            .unwrap_or_default()
    }

    /// Answer an agent's call. Nothing happens when nobody waits for it any
    /// more.
    pub fn answer_browser_call(&mut self, call: u64, reply: BrowserReply) {
        if let Some(tools) = self.browser_tools.as_ref() {
            tools.bridge.answer(call, reply);
        }
    }

    /// A worker's window answered call `call`: only the window it was sent
    /// to may.
    pub(super) fn window_answered_browser_call(
        &mut self,
        viewer: &ViewerId,
        call: u64,
        reply: BrowserReply,
    ) -> bool {
        let Some(tools) = self.browser_tools.as_mut() else {
            return false;
        };
        if tools.sent.get(&call) != Some(viewer) {
            return false;
        }
        tools.sent.remove(&call);
        tools.bridge.answer(call, reply)
    }

    /// The launch every agent gets, while browser tools are on.
    pub fn browser_mcp_launch(&self) -> Option<&BrowserMcpLaunch> {
        self.browser_tools.as_ref().map(|t| &t.launch)
    }
}
