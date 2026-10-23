//! [`AppHost`]'s worker mode: the host `flightdeck worker run` drives (SPECS
//! §33, `specs/WORKER_PRD.md` §4.4).
//!
//! A worker host is an ordinary [`AppHost`] with extras: it may hold no
//! project at all (a fresh worker), never starts FlightDeck Remote, serves the
//! web protocol on Tailscale only, keeps a login shell for the attached
//! window, and confines project browsing, opening and cloning to `$HOME`. A
//! child of [`crate::host`] so it can reach the host's private state.
//!
//! The run loop calls only [`AppHost::open_worker`],
//! [`AppHost::resume_all_projects`], [`AppHost::start_worker`],
//! [`AppHost::tick`], the worker API below, [`AppHost::persist`],
//! [`AppHost::terminate_sessions`] and [`AppHost::stop_worker`]: nothing else
//! is guaranteed safe on an empty workspace.

// stub: filled by C (WorkerExt, the empty-workspace guards, inbound routing,
// and every method below)

use std::path::PathBuf;
use std::sync::Arc;

use crate::contracts::{Config, FlightDeckError, Result, WorkerConfig};
use crate::host::AppHost;
use crate::worker::control::{DeviceReport, PairingCode};
use crate::worker::paths::WorkerPaths;
use crate::worker::status::{ListenReport, WorkerRuntimeReport};
use crate::Env;

pub mod clone;
pub mod health;
pub mod messages;
pub mod notify;
pub mod projects;
pub mod shell;

pub use notify::WorkerNotifier;

/// Everything a worker host needs beyond the services in [`Env`].
pub struct WorkerOptions {
    /// Where the worker workspace file and the device tokens live.
    pub paths: WorkerPaths,
    /// The confinement root for browsing/opening/cloning, and the worker
    /// shell's working directory.
    pub home: PathBuf,
    pub config: WorkerConfig,
    /// The effective global config: what an empty workspace uses where a
    /// project's config would otherwise answer.
    pub global_config: Config,
    pub health: Box<dyn health::HealthProbe + Send>,
    pub cloner: Arc<dyn clone::CloneRunner>,
}

impl<'a> AppHost<'a> {
    /// Open the worker workspace (it may be empty): reopen the projects
    /// remembered in `opts.paths.workspace`, seed every project's PTY size
    /// to `default_cols`×`default_rows`, never start FlightDeck Remote, and
    /// capture what the TUI would show in its status line as host messages.
    pub fn open_worker(
        _env: Env<'a>,
        _notifier: &'a WorkerNotifier,
        _opts: WorkerOptions,
    ) -> Result<AppHost<'a>> {
        // stub: filled by C
        Err(FlightDeckError::Other(
            "worker mode is not implemented in this build yet".to_string(),
        ))
    }

    /// Resume the agents of every open project (a worker has no "active"
    /// screen: a remote window browses each project on its own).
    pub fn resume_all_projects(&mut self) {
        // stub: filled by C
    }

    /// Start the web server (Tailscale listeners, no browser assets) and the
    /// worker shell. A bind failure is not fatal: the worker keeps running,
    /// not listening, and retries.
    pub fn start_worker(&mut self) {
        // stub: filled by C
    }

    /// What `worker status` shows about the running worker.
    pub fn worker_report(&self) -> WorkerRuntimeReport {
        // stub: filled by C
        WorkerRuntimeReport {
            version: crate::VERSION.to_string(),
            pid: std::process::id(),
            uptime_secs: 0,
            listen: ListenReport {
                listening: false,
                addresses: Vec::new(),
                advertised: None,
                reason: None,
                origin: None,
            },
            devices: Vec::new(),
            attached: None,
            background_links: 0,
            projects: Vec::new(),
            unread_messages: 0,
        }
    }

    /// Mint a single-use pairing code (`worker pair`).
    pub fn mint_pairing_code(&mut self) -> PairingCode {
        // stub: filled by C
        PairingCode {
            code: String::new(),
            expires_in_secs: 0,
            address: None,
            listening: false,
            not_listening_reason: None,
        }
    }

    /// Every paired Desktop, with its connection state.
    pub fn devices(&self) -> Vec<DeviceReport> {
        // stub: filled by C
        Vec::new()
    }

    /// Revoke the Desktop `selector` names (an exact id, an exact label, or a
    /// unique id prefix) and close its connections; returns the device and
    /// how many connections were closed. `Err` names the candidates when the
    /// selector is ambiguous.
    pub fn revoke_device(
        &mut self,
        _selector: &str,
    ) -> std::result::Result<(DeviceReport, u32), String> {
        // stub: filled by C
        Err("revoking a device is not implemented in this build yet".to_string())
    }

    /// Record whether the coming stop is a restart, so connections close
    /// with "restarting" rather than "worker stopped".
    pub fn set_shutdown_intent(&mut self, _restarting: bool) {
        // stub: filled by C
    }

    /// Close every connection (as a stop or a restart, per
    /// [`AppHost::set_shutdown_intent`]) and stop the worker shell.
    pub fn stop_worker(&mut self) {
        // stub: filled by C
    }

    /// Lines for the worker log since the last call: listen changes,
    /// takeovers, host messages.
    pub fn take_log_lines(&mut self) -> Vec<String> {
        // stub: filled by C
        Vec::new()
    }
}
