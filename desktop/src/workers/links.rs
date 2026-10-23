//! A background link to every saved worker, for as long as the app runs
//! (`specs/WORKER_PRD.md` §4.5, W18, W19).
//!
//! [`WorkerLinks`] is one entity for the whole process, published as the
//! [`WorkerLinksHandle`] global and started by `app::run` in every mode
//! (launcher, a local project, isolated). Every [`RELOAD_EVERY`] it re-reads
//! `~/.flightdeck/remotes.json` and keeps exactly one status-only link
//! ([`ConnectionKind::Background`], observe seat, reconnect backoff capped at
//! a minute) per saved remote whose host said it is a worker: a newly saved
//! one is dialled, a forgotten one is dropped, a re-paired one (new token) is
//! dialled again. Every [`PUMP_EVERY`] it applies what the links said, on
//! GPUI's foreground executor, and the launcher's Workers section redraws
//! from [`WorkerLinks::rows`].
//!
//! ## Notifications
//!
//! A worker raises the notifications a local host would (its own
//! `take_finish_notifications`, design A7); this decides where they show.
//! Several Desktop processes may run on one machine (File ▸ New window starts
//! another), and each holds its own links, so only the one holding
//! `~/.flightdeck/desktop-links.lock` posts ([`Ownership`]; the others retry
//! every [`LOCK_RETRY`], so one takes over when the owner quits). A
//! notification is not posted while a remote window to that worker is open in
//! this process ([`registry::is_open`]) or in another process on this machine
//! (the worker reports its window as `same_device`): that window notifies
//! instead, so nothing is doubled.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use flightdeck::contracts::{FileLock, FileSystem, Notification, NotificationSound, Notifier};
use flightdeck::web::client::store::{RemotesFile, SavedRemote};
use flightdeck::web::client::{client_label, LinkConfig, LinkEnd, LinkState, RemoteClient};
use flightdeck::web::protocol::{
    ConnectionKind, NotificationSoundView, NotificationView, SeatRequest,
};
use flightdeck_desktop::workers::registry;
use gpui::{App, AppContext, Context, Entity, Global, Task};

use super::view::{WorkerRow, WorkerState};
use crate::remote::connect::{OpenRemote, Store};

/// How often the links' news is applied.
pub const PUMP_EVERY: Duration = Duration::from_millis(500);

/// How often `remotes.json` is re-read for workers saved or forgotten.
pub const RELOAD_EVERY: Duration = Duration::from_secs(5);

/// How often a process that does not post notifications tries to become the
/// one that does.
pub const LOCK_RETRY: Duration = Duration::from_secs(30);

/// The longest a background link waits between reconnect attempts, in ms.
pub const RETRY_CAP_MS: u64 = 60_000;

/// The lock file whose holder posts worker notifications, beside
/// `remotes.json` in `~/.flightdeck/`.
pub const LOCK_FILE: &str = "desktop-links.lock";

/// Starts a link for a saved worker: the real one dials, tests script it.
pub type Dial = Rc<dyn Fn(&SavedRemote) -> RemoteClient>;

/// The link a saved worker is watched over: background, observing, backing
/// off to [`RETRY_CAP_MS`], and labelled with this machine's name so the
/// worker's device list says which Desktop it is (design A11).
pub fn link_config(remote: &SavedRemote) -> LinkConfig {
    LinkConfig {
        address: remote.address.clone(),
        token: remote.access_token(),
        seat: SeatRequest::Observe,
        user_agent: client_label(
            flightdeck::web::server::NATIVE_CLIENT_AGENT,
            env!("CARGO_PKG_VERSION"),
        ),
        connection: ConnectionKind::Background,
        retry_cap_ms: Some(RETRY_CAP_MS),
    }
}

/// The real dial: [`RemoteClient::connect`] on [`link_config`].
fn real_dial() -> Dial {
    Rc::new(|remote| RemoteClient::connect(link_config(remote)))
}

/// Whether this process is the one that posts worker notifications: it holds
/// [`LOCK_FILE`]. The lock is the OS's, so a crashed owner leaves none behind.
pub struct Ownership {
    fs: Rc<dyn FileSystem>,
    path: Option<PathBuf>,
    held: Option<FileLock>,
    last_attempt: Option<Instant>,
}

impl Ownership {
    /// The lock at `path` (`None`: no home directory, so never the owner).
    pub fn new(fs: Rc<dyn FileSystem>, path: Option<PathBuf>) -> Ownership {
        Ownership {
            fs,
            path,
            held: None,
            last_attempt: None,
        }
    }

    /// `~/.flightdeck/desktop-links.lock` on the real filesystem.
    fn real() -> Ownership {
        let path = flightdeck::web::client::store::remotes_path()
            .map(|remotes| remotes.with_file_name(LOCK_FILE));
        Ownership::new(Rc::new(flightdeck::contracts::real::RealFs), path)
    }

    /// Take the lock if it is not held yet and the last try is
    /// [`LOCK_RETRY`] old. Returns whether this process owns it now.
    pub fn refresh(&mut self, now: Instant) -> bool {
        if self.held.is_some() {
            return true;
        }
        let due = self
            .last_attempt
            .is_none_or(|last| now.duration_since(last) >= LOCK_RETRY);
        if let (true, Some(path)) = (due, &self.path) {
            self.last_attempt = Some(now);
            // An error (an unwritable home) is "not the owner"; try again
            // later like a held lock.
            self.held = self.fs.try_lock_exclusive(path).ok().flatten();
        }
        self.held.is_some()
    }

    /// Whether this process posts.
    pub fn owns(&self) -> bool {
        self.held.is_some()
    }
}

/// One saved worker and its link.
struct Link {
    saved: SavedRemote,
    client: RemoteClient,
    /// When this process last saw the link live, Unix seconds.
    last_live: Option<u64>,
    /// When the link ended for good, so one that might recover by itself
    /// (the worker was updated) is dialled again after [`RETRY_CAP_MS`].
    ended_at: Option<Instant>,
    /// What the row said before a re-dial, shown while it connects again.
    previous: Option<WorkerState>,
}

/// The process's background links. See the module docs.
pub struct WorkerLinks {
    store: Store,
    dial: Dial,
    open: OpenRemote,
    notifier: Rc<dyn Notifier>,
    ownership: Ownership,
    links: Vec<Link>,
    last_reload: Option<Instant>,
    _ticker: Option<Task<()>>,
}

/// The app's [`WorkerLinks`].
pub struct WorkerLinksHandle(pub Entity<WorkerLinks>);

impl Global for WorkerLinksHandle {}

/// Start the app's links: the real remotes file, dial, notifier and lock,
/// driven by their own timer, published as [`WorkerLinksHandle`].
pub fn start(cx: &mut App) {
    let links = cx.new(|cx| {
        let mut links = WorkerLinks::new(
            Store::real(),
            real_dial(),
            Rc::new(crate::remote::open_remote_window),
            Rc::new(flightdeck::notify::SystemNotifier),
            Ownership::real(),
        );
        links.start_ticking(cx);
        links
    });
    cx.set_global(WorkerLinksHandle(links));
}

/// Open the remote window for the worker saved at `address`, with the
/// writing seat, as a saved remote is opened from Connect to remote….
pub fn open(address: &str, cx: &mut App) {
    let Some(handle) = cx.try_global::<WorkerLinksHandle>().map(|h| h.0.clone()) else {
        return;
    };
    let (open, saved) = {
        let links = handle.read(cx);
        (links.open.clone(), links.saved(address))
    };
    if let Some(saved) = saved {
        open(saved, SeatRequest::Write, cx);
    }
}

/// The rows of the app's links, for the launcher; empty before they start.
pub fn rows(cx: &App) -> Vec<WorkerRow> {
    cx.try_global::<WorkerLinksHandle>()
        .map(|h| h.0.read(cx).rows())
        .unwrap_or_default()
}

impl WorkerLinks {
    /// Links over `store`'s workers. Nothing is dialled until the first
    /// [`WorkerLinks::turn`].
    pub fn new(
        store: Store,
        dial: Dial,
        open: OpenRemote,
        notifier: Rc<dyn Notifier>,
        ownership: Ownership,
    ) -> WorkerLinks {
        WorkerLinks {
            store,
            dial,
            open,
            notifier,
            ownership,
            links: Vec::new(),
            last_reload: None,
            _ticker: None,
        }
    }

    /// Spawn the turn loop on GPUI's foreground executor.
    pub fn start_ticking(&mut self, cx: &mut Context<Self>) {
        self._ticker = Some(cx.spawn(async move |this, cx| loop {
            if this.update(cx, |links, cx| links.turn(cx)).is_err() {
                break;
            }
            cx.background_executor().timer(PUMP_EVERY).await;
        }));
    }

    /// One turn: re-read the saved workers when due, claim the notification
    /// lock when due, apply what the links said, post what they raised, and
    /// redraw whoever observes this when anything changed.
    pub fn turn(&mut self, cx: &mut Context<Self>) -> bool {
        let now = Instant::now();
        let mut changed = false;
        if self
            .last_reload
            .is_none_or(|last| now.duration_since(last) >= RELOAD_EVERY)
        {
            self.last_reload = Some(now);
            changed |= self.reload(now);
        }
        self.ownership.refresh(now);
        changed |= self.pump(now, cx);
        if changed {
            cx.notify();
        }
        changed
    }

    /// Re-read `remotes.json` and [`WorkerLinks::sync`] to it.
    pub fn reload(&mut self, now: Instant) -> bool {
        let file = self.store.load();
        self.sync(&file, now)
    }

    /// One link per worker in `file`, in its order: dial new ones, drop
    /// forgotten ones, re-dial one whose token changed (it was paired again)
    /// or whose link ended in a way a retry may fix. Returns whether the list
    /// changed.
    pub fn sync(&mut self, file: &RemotesFile, now: Instant) -> bool {
        let before: Vec<String> = self.links.iter().map(|l| l.saved.address.clone()).collect();
        let mut old = std::mem::take(&mut self.links);
        let mut changed = false;
        for saved in file.workers() {
            let kept = old
                .iter()
                .position(|l| l.saved.address == saved.address)
                .map(|i| old.swap_remove(i));
            let link = match kept {
                Some(link) if link.saved.token != saved.token => {
                    changed = true;
                    self.dial(saved, None)
                }
                Some(link) if redial_due(&link, now) => {
                    changed = true;
                    let previous = Some(link_state(&link));
                    self.dial(saved, previous)
                }
                Some(mut link) => {
                    if link.saved != *saved {
                        changed = true;
                        link.saved = saved.clone();
                    }
                    link
                }
                None => self.dial(saved, None),
            };
            self.links.push(link);
        }
        // Dropping a client closes its link without waiting on the socket.
        drop(old);
        let after: Vec<&str> = self
            .links
            .iter()
            .map(|l| l.saved.address.as_str())
            .collect();
        changed || before != after
    }

    fn dial(&self, saved: &SavedRemote, previous: Option<WorkerState>) -> Link {
        Link {
            client: (self.dial)(saved),
            saved: saved.clone(),
            last_live: None,
            ended_at: None,
            previous,
        }
    }

    /// Apply every link's news and post the notifications it raised.
    fn pump(&mut self, now: Instant, cx: &App) -> bool {
        let owner = self.ownership.owns();
        let mut changed = false;
        for link in &mut self.links {
            changed |= link.client.pump();
            match link.client.state() {
                LinkState::Live { .. } => {
                    link.last_live = Some(unix_now());
                    link.previous = None;
                }
                LinkState::Ended(_) if link.ended_at.is_none() => link.ended_at = Some(now),
                _ => {}
            }
            // Taken every turn, so a process that does not post does not
            // hoard them.
            let raised = link.client.take_notifications();
            if !owner || raised.is_empty() || window_elsewhere(link, cx) {
                continue;
            }
            for notification in &raised {
                self.notifier
                    .notify(&worker_notification(&link.saved.label, notification));
            }
        }
        changed
    }

    /// The saved record for `address`, fresh from the file (a re-pair may
    /// have changed its token since the last reload).
    fn saved(&self, address: &str) -> Option<SavedRemote> {
        self.store.load().get(address).cloned().or_else(|| {
            self.links
                .iter()
                .find(|l| l.saved.address == address)
                .map(|l| l.saved.clone())
        })
    }

    /// One row per saved worker, in the file's order.
    pub fn rows(&self) -> Vec<WorkerRow> {
        self.links.iter().map(row).collect()
    }

    /// Whether this process posts worker notifications.
    #[cfg(test)]
    pub fn owns_notifications(&self) -> bool {
        self.ownership.owns()
    }

    /// Let the next turn try the notification lock without waiting
    /// [`LOCK_RETRY`].
    #[cfg(test)]
    pub fn retry_ownership_now(&mut self) {
        self.ownership.last_attempt = None;
    }
}

/// Whether a window on this worker notifies instead of the background link:
/// one in this process, or one this machine's credential holds elsewhere.
fn window_elsewhere(link: &Link, cx: &App) -> bool {
    if registry::is_open(&link.saved.address, cx) {
        return true;
    }
    link.client
        .workspace()
        .worker
        .as_ref()
        .and_then(|w| w.connections.window.as_ref())
        .is_some_and(|window| window.same_device)
}

/// A worker's notification as this machine posts it: the title names the
/// worker.
pub fn worker_notification(label: &str, raised: &NotificationView) -> Notification {
    Notification {
        title: format!("{label}: {}", raised.title),
        body: raised.body.clone(),
        sound: match raised.sound {
            NotificationSoundView::None => NotificationSound::None,
            NotificationSoundView::Completion => NotificationSound::Completion,
            NotificationSoundView::InputRequired => NotificationSound::InputRequired,
        },
    }
}

/// Whether an ended link should be dialled again: not for a token the worker
/// refused (only pairing again fixes that, and it changes the token), but a
/// version mismatch goes away when either side is updated.
fn redial_due(link: &Link, now: Instant) -> bool {
    let recoverable = !matches!(
        link.client.state(),
        LinkState::Ended(LinkEnd::Revoked | LinkEnd::Unauthorized)
    );
    recoverable
        && link
            .ended_at
            .is_some_and(|at| now.duration_since(at) >= Duration::from_millis(RETRY_CAP_MS))
}

/// The row's state from the link's.
fn link_state(link: &Link) -> WorkerState {
    let offline = WorkerState::Offline {
        last_seen: link.last_live.or(link.saved.last_seen),
    };
    match link.client.state() {
        LinkState::Live { .. } => WorkerState::Online,
        LinkState::Connecting => link.previous.clone().unwrap_or(WorkerState::Connecting),
        LinkState::Reconnecting { .. } => offline,
        LinkState::Ended(LinkEnd::Unauthorized) => WorkerState::NotPaired,
        LinkState::Ended(LinkEnd::Revoked) => WorkerState::Revoked,
        LinkState::Ended(LinkEnd::VersionMismatch(why)) => WorkerState::UpdateNeeded(why.clone()),
        LinkState::Ended(
            LinkEnd::HostQuit
            | LinkEnd::ServerStopped
            | LinkEnd::TakenOver { .. }
            | LinkEnd::Stopped,
        ) => offline,
    }
}

fn row(link: &Link) -> WorkerRow {
    let state = link_state(link);
    let ws = link.client.workspace();
    let online = state == WorkerState::Online;
    let worker = ws.worker.as_ref();
    let health = worker.and_then(|w| w.health.clone());
    let version = health
        .as_ref()
        .map(|h| h.version.clone())
        .or_else(|| (ws.ready && !ws.host_version.is_empty()).then(|| ws.host_version.clone()))
        .or_else(|| link.saved.host_version.clone())
        .filter(|v| !v.is_empty());
    WorkerRow {
        label: link.saved.label.clone(),
        address: link.saved.address.clone(),
        needs_you: if online { ws.needs_you_count() } else { 0 },
        skew: version.as_deref().is_some_and(|v| v != flightdeck::VERSION),
        version,
        health: health.filter(|_| online),
        unread: worker.map_or(0, |w| w.status.unread_messages),
        state,
    }
}

/// The wall clock, Unix seconds.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
