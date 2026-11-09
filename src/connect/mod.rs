//! `flightdeck connect`: the terminal UI driving a FlightDeck on another
//! machine — a worker, or another machine's terminal UI or Desktop
//! (`remote-control-ki05`; `specs/WORKER_PRD.md` §9).
//!
//! It runs no local host. It is a native client of FlightDeck Web exactly like
//! FlightDeck Desktop's remote window — the same core
//! [`crate::web::client::session::RemoteSession`] — drawn with the local TUI's
//! widgets ([`ui`]). One remote per run (R4), never mixed with local projects.
//!
//! Pairing is the Desktop's: the host's address and the 4-digit code it shows
//! (`flightdeck worker pair` on a worker, the web access overlay on a terminal
//! UI or Desktop host), exchanged for a token saved in
//! `~/.flightdeck/remotes.json` — the very file the Desktop keeps, so a remote
//! paired from either is known to both.
//!
//! On a worker it attaches as a window: like a Desktop window, it takes the
//! worker over from any window already open on it (W17), and sizes the
//! worker's PTYs to its own terminal pane (W5).

pub mod ui;

#[cfg(test)]
mod tests;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::rc::Rc;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::layout::Rect;

use crate::contracts::real::RealFs;
use crate::contracts::{Clock, UiConfig};
use crate::web::client::store::{
    load_remotes, remotes_path, save_remotes, RemotesFile, SavedRemote,
};
use crate::web::client::{
    client_label, exchange_code, link_is_unprotected, normalize_address, LinkConfig, RemoteClient,
};
use crate::web::protocol::{ConnectionKind, SeatRequest};
use crate::web::server::NATIVE_TERMINAL_AGENT;

use ui::{ConnectUi, Flow};

const USAGE: &str = "\
flightdeck connect — drive a FlightDeck on another machine from this terminal

USAGE:
    flightdeck connect [<name or host:port>] [--observe]
    flightdeck connect --pair <host[:port]> [--observe]
    flightdeck connect --list

With no argument, pick one of the remotes paired before (from this terminal
or from FlightDeck Desktop). A FlightDeck worker is reached at its Tailscale
address; port 7420 is the default.

OPTIONS:
    --pair <host[:port]>   Pair with a host first: type the 4-digit code that
                           `flightdeck worker pair` (or the host's browser
                           access overlay) shows
    --observe              Watch without typing
    --list                 List the paired remotes and exit
    -h, --help             Print this help";

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Request {
    Help,
    List,
    /// Connect to a saved remote, picked by name or address, or asked for.
    Connect {
        target: Option<String>,
        seat: SeatRequest,
    },
    /// Pair with `address` first, then connect.
    Pair {
        address: String,
        seat: SeatRequest,
    },
}

/// Parse `flightdeck connect`'s arguments (those after `connect`).
pub(crate) fn parse(args: &[String]) -> Result<Request, String> {
    let mut seat = SeatRequest::Write;
    let mut pair: Option<String> = None;
    let mut target: Option<String> = None;
    let mut list = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Request::Help),
            "--list" => list = true,
            "--observe" => seat = SeatRequest::Observe,
            "--pair" => {
                let address = it
                    .next()
                    .ok_or("--pair needs the host's address, e.g. --pair 100.64.0.2")?;
                pair = Some(address.clone());
            }
            other if other.starts_with('-') => {
                return Err(format!(
                    "unknown option `{other}`; see `flightdeck connect --help`"
                ))
            }
            other if target.is_none() => target = Some(other.to_string()),
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    if list {
        return Ok(Request::List);
    }
    match (pair, target) {
        (Some(_), Some(_)) => Err("give either --pair <address> or a remote, not both".into()),
        (Some(raw), None) => {
            let address = normalize_address(&raw)
                .ok_or_else(|| format!("`{raw}` is not an address (host or host:port)"))?;
            Ok(Request::Pair { address, seat })
        }
        (None, target) => Ok(Request::Connect { target, seat }),
    }
}

/// The saved remote `target` names: its label (any case) or its address.
pub(crate) fn find<'a>(saved: &'a RemotesFile, target: &str) -> Option<&'a SavedRemote> {
    let wanted = target.trim();
    saved
        .remotes
        .iter()
        .find(|r| r.label.eq_ignore_ascii_case(wanted))
        .or_else(|| {
            let address = normalize_address(wanted)?;
            saved.remotes.iter().find(|r| r.address == address)
        })
}

/// The label a new remote gets: its host name or address, without the port.
pub(crate) fn host_label(address: &str) -> String {
    match address.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest).to_string(),
        None => address
            .rsplit_once(':')
            .map_or(address, |(host, _)| host)
            .to_string(),
    }
}

/// `flightdeck connect …`. Returns the process exit code.
pub fn main(args: &[String]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(message) => {
            crate::best_effort_stderr_line(&format!("flightdeck connect: {message}"));
            1
        }
    }
}

/// Where remotes are saved, and the filesystem they are saved on.
struct Store {
    fs: RealFs,
    path: Option<PathBuf>,
}

impl Store {
    fn load(&self) -> RemotesFile {
        self.path
            .as_deref()
            .map(|p| load_remotes(&self.fs, p))
            .unwrap_or_default()
    }

    fn save(&self, file: &RemotesFile) {
        if let Some(path) = &self.path {
            if let Err(e) = save_remotes(&self.fs, path, file) {
                crate::best_effort_stderr_line(&format!(
                    "flightdeck connect: could not save {}: {e}",
                    path.display()
                ));
            }
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let request = parse(args)?;
    let store = Store {
        fs: RealFs,
        path: remotes_path(),
    };
    let (remote, seat) = match request {
        Request::Help => {
            println!("{USAGE}");
            return Ok(());
        }
        Request::List => {
            list(&store.load());
            return Ok(());
        }
        Request::Pair { address, seat } => (pair(&store, &address)?, seat),
        Request::Connect { target, seat } => (choose(&store, target.as_deref())?, seat),
    };
    if !confirm_unencrypted(&store, &remote)? {
        return Ok(());
    }
    drive(&store, remote, seat).map_err(|e| e.to_string())
}

fn list(saved: &RemotesFile) {
    if saved.remotes.is_empty() {
        println!("No remotes paired yet. Pair with: flightdeck connect --pair <host[:port]>");
        return;
    }
    for remote in saved.by_recent() {
        let kind = if remote.is_worker() { " (worker)" } else { "" };
        println!("{}{kind}  {}", remote.label, remote.address);
    }
}

/// The saved remote to connect to: the one `target` names, the only one, or
/// the one picked from a numbered list.
fn choose(store: &Store, target: Option<&str>) -> Result<SavedRemote, String> {
    let saved = store.load();
    if let Some(target) = target {
        return find(&saved, target).cloned().ok_or_else(|| {
            format!("not paired with `{target}`. Pair first: flightdeck connect --pair {target}")
        });
    }
    let remotes = saved.by_recent();
    match remotes.len() {
        0 => {
            Err("no remotes paired yet. Pair with: flightdeck connect --pair <host[:port]>".into())
        }
        1 => Ok(remotes[0].clone()),
        _ => {
            println!("Paired remotes:");
            for (i, remote) in remotes.iter().enumerate() {
                let kind = if remote.is_worker() { " (worker)" } else { "" };
                println!("  {}. {}{kind}  {}", i + 1, remote.label, remote.address);
            }
            let answer = prompt("Connect to which? [1] ")?;
            let index = if answer.is_empty() {
                0
            } else {
                answer
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .filter(|&i| i < remotes.len())
                    .ok_or_else(|| format!("`{answer}` is not one of the numbers above"))?
            };
            Ok(remotes[index].clone())
        }
    }
}

/// Exchange the host's 4-digit code for a token and save the remote.
fn pair(store: &Store, address: &str) -> Result<SavedRemote, String> {
    println!("Pairing with {address}.");
    println!(
        "On a worker, run `flightdeck worker pair` there; on a terminal UI or Desktop host, open \
         browser access in network mode."
    );
    let code = prompt("Code: ")?;
    if code.len() != 4 || !code.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("`{code}` is not a 4-digit code"));
    }
    let runtime =
        crate::remote::runtime::try_shared().ok_or("the async runtime could not start")?;
    let label = client_label(NATIVE_TERMINAL_AGENT, crate::VERSION);
    let (addr, code_owned) = (address.to_string(), code.clone());
    let token = runtime
        .handle()
        .block_on(async move { exchange_code(&addr, &code_owned, &label).await })
        .map_err(|e| e.sentence())?;
    let mut saved = store.load();
    let label = saved
        .get(address)
        .map(|r| r.label.clone())
        .unwrap_or_else(|| host_label(address));
    let remote = SavedRemote {
        label,
        address: address.to_string(),
        token: token.reveal().to_string(),
        last_seen: None,
        viewer_id: None,
        host_version: None,
        warned_unencrypted: false,
        ssh_target: None,
        host_kind: None,
    };
    saved.upsert(remote);
    store.save(&saved);
    println!("Paired.");
    // `upsert` keeps a re-paired remote's own settings (its name, its SSH
    // target), so read back what was saved.
    Ok(saved
        .get(address)
        .cloned()
        .expect("the remote was just saved"))
}

/// The plain-WebSocket warning, once per remote, unless the link is loopback
/// or Tailscale (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §4). Whether to go on.
fn confirm_unencrypted(store: &Store, remote: &SavedRemote) -> Result<bool, String> {
    if remote.warned_unencrypted || !link_is_unprotected(&remote.address) {
        return Ok(true);
    }
    println!(
        "The link to {} is a plain WebSocket: anyone on this network can read what you type \
         and see. Use a Tailscale address (100.x.y.z) where you can.",
        remote.address
    );
    let answer = prompt("Connect anyway? [y/N] ")?;
    if !answer.eq_ignore_ascii_case("y") && !answer.eq_ignore_ascii_case("yes") {
        return Ok(false);
    }
    let mut saved = store.load();
    if let Some(mut r) = saved.get(&remote.address).cloned() {
        r.warned_unencrypted = true;
        saved.upsert(r);
        store.save(&saved);
    }
    Ok(true)
}

/// Ask on stdout, read one line from stdin.
fn prompt(question: &str) -> Result<String, String> {
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("could not read the answer: {e}"))?;
    Ok(line.trim().to_string())
}

/// This machine's `[ui]` settings, from the global config: how this terminal
/// looks is this machine's business, the host's projects' settings are the
/// host's.
fn local_ui() -> UiConfig {
    crate::config::load::global_config_path()
        .and_then(|path| crate::config::load::load_config(&RealFs, &path).ok())
        .map(|config| config.ui)
        .unwrap_or_default()
}

/// Run the remote UI on this terminal until it quits.
fn drive(store: &Store, remote: SavedRemote, seat: SeatRequest) -> crate::Result<()> {
    let config = LinkConfig {
        address: remote.address.clone(),
        token: remote.access_token(),
        seat,
        // A worker shows this machine's name in its device list and in
        // "taken over by …" (design A11).
        user_agent: client_label(NATIVE_TERMINAL_AGENT, crate::VERSION),
        connection: ConnectionKind::Window,
        retry_cap_ms: None,
    };
    let client = RemoteClient::connect(config.clone());
    let mut ui = ConnectUi::new(
        client,
        &remote.label,
        &remote.address,
        remote.host_kind,
        local_ui(),
    );
    ui.set_dialer(Rc::new(move || RemoteClient::connect(config.clone())));
    ui.set_ssh_target(remote.ssh_target.clone());

    let mut terminal = crate::init_terminal().map_err(|e| {
        crate::contracts::error::FlightDeckError::Io(format!("failed to initialise terminal: {e}"))
    })?;
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    let keyboard_enhanced = matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    );
    if keyboard_enhanced {
        let _ = crossterm::execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    let _ = crate::save_and_set_terminal_title(&format!("flightdeck — {}", remote.label));
    std::panic::set_hook(Box::new(move |info| {
        if crate::terminal::session::parser_panic_expected() {
            return;
        }
        restore(keyboard_enhanced);
        let _ = ratatui::try_restore();
        crate::best_effort_stderr_line(&format!("FlightDeck panicked: {info}"));
    }));

    let result = event_loop(&mut terminal, &mut ui, store, &remote.address);

    restore(keyboard_enhanced);
    let _ = ratatui::try_restore();
    let _ = terminal.show_cursor();
    std::mem::forget(terminal);
    result
}

/// Undo the terminal modes [`drive`] turned on.
fn restore(keyboard_enhanced: bool) {
    if keyboard_enhanced {
        let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    let _ = crate::restore_terminal_title();
}

fn event_loop(
    terminal: &mut crate::TuiTerminal,
    ui: &mut ConnectUi,
    store: &Store,
    address: &str,
) -> crate::Result<()> {
    let shutdown = crate::signals::install_shutdown_flag();
    // Input on its own thread, as the local TUI reads it, so a severed
    // terminal cannot wedge the loop (see `event_loop` in the crate root).
    let (input_tx, input_rx) = std::sync::mpsc::channel::<Event>();
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            if input_tx.send(event).is_err() {
                break;
            }
        }
    });
    let clock = crate::contracts::real::RealClock;
    let mut recorded = false;
    loop {
        let area = match terminal.size() {
            Ok(size) => Rect::new(0, 0, size.width, size.height),
            Err(_) => Rect::new(0, 0, 80, 24),
        };
        ui.pump(area);
        if !recorded && ui.session().workspace().ready {
            recorded = true;
            record_attach(store, address, ui, clock.now_millis() / 1000);
        }
        if let Some(text) = ui.take_copied() {
            crate::tui::clipboard::copy(&text);
        }
        for url in ui.take_open_urls() {
            let _ = crate::tui::opener::open_url(&url);
        }
        if ui.should_quit() {
            break;
        }
        let now_ms = clock.now_millis();
        terminal.draw(|frame| ui.draw(frame, now_ms)).map_err(|e| {
            crate::contracts::error::FlightDeckError::Io(format!("render failed: {e}"))
        })?;

        let event = match crate::next_loop_step(&shutdown, &input_rx, crate::POLL_TIMEOUT) {
            crate::LoopStep::Shutdown => break,
            crate::LoopStep::Idle => continue,
            crate::LoopStep::Input(event) => event,
        };
        let flow = match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => ui.key(key),
            Event::Mouse(me) => {
                ui.mouse(me, area);
                Flow::Continue
            }
            Event::Paste(text) => {
                ui.paste(text);
                Flow::Continue
            }
            _ => Flow::Continue,
        };
        if flow == Flow::Quit {
            break;
        }
    }
    Ok(())
}

/// Remember this attach in the saved remotes: when, the host's version and
/// viewer id, and what kind of FlightDeck answered (so a worker is listed as
/// one, here and in the Desktop's launcher).
fn record_attach(store: &Store, address: &str, ui: &ConnectUi, now_secs: u64) {
    let ws = ui.session().workspace();
    let mut saved = store.load();
    saved.touch(
        address,
        now_secs,
        ws.viewer_id.as_ref().map(|v| v.to_string()),
        Some(ws.host_version.clone()),
    );
    if let Some(kind) = ws.host_kind {
        saved.record_host_kind(address, kind);
    }
    store.save(&saved);
}
