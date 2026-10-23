//! The Workers section and its background links, driven through GPUI's
//! headless test platform: `remotes.json` on the core's fake filesystem, each
//! link a scripted client (`RemoteClient::scripted`), notifications into a
//! recording notifier. No socket, no OS notification.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use flightdeck::contracts::{
    AgentTabPosition, FileSystem, InterpretedStatus, NotificationSound, Notifier, TabId,
};
use flightdeck::testing::{FakeFs, FakeNotifier};
use flightdeck::web::client::store::{save_remotes, RemotesFile, SavedRemote};
use flightdeck::web::client::{LinkEnd, LinkEvent, LinkState, RemoteClient, Script};
use flightdeck::web::protocol::{
    ConnectionKind, ConnectionsView, Delta, Geometry, GitBar, Health, HostKind,
    NotificationSoundView, NotificationView, ProjectView, Seat, SeatRequest, Selection,
    SessionPhase, SessionStatus, SessionView, Snapshot, StatusBucket, WindowInfo, WorkerSnapshot,
    WorkerStatusView, PROTOCOL_VERSION,
};
use flightdeck_desktop::workers::registry;
use gpui::{AppContext, Entity, Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;

use super::links::{link_config, Ownership, WorkerLinks, WorkerLinksHandle, RETRY_CAP_MS};
use super::view::{WorkerRow, WorkerState};
use crate::remote::connect::Store;
use crate::root::{AppRoot, Opener};

const REMOTES: &str = "/home/u/.flightdeck/remotes.json";
const LOCK: &str = "/home/u/.flightdeck/desktop-links.lock";
const BOX: &str = "box.ts.net:7420";

/// Everything a test drives the links through.
struct Rig {
    fs: Rc<FakeFs>,
    notifier: Rc<FakeNotifier>,
    /// Every link dialled, in order, with its saved address.
    scripts: Rc<RefCell<Vec<(String, Script)>>>,
    /// Every remote window the section opened.
    opened: Rc<RefCell<Vec<(String, SeatRequest)>>>,
}

impl Rig {
    fn new() -> Rig {
        Rig {
            fs: Rc::new(FakeFs::new()),
            notifier: Rc::new(FakeNotifier::new()),
            scripts: Rc::default(),
            opened: Rc::default(),
        }
    }

    fn save(&self, remotes: Vec<SavedRemote>) {
        let file = RemotesFile {
            version: 1,
            remotes,
        };
        save_remotes(self.fs.as_ref(), Path::new(REMOTES), &file).unwrap();
    }

    fn links(&self, app: &mut TestAppContext) -> Entity<WorkerLinks> {
        let fs: Rc<dyn FileSystem> = self.fs.clone();
        let store = Store {
            fs: fs.clone(),
            path: Some(PathBuf::from(REMOTES)),
        };
        let scripts = self.scripts.clone();
        let dial = Rc::new(move |saved: &SavedRemote| {
            let (client, script) = RemoteClient::scripted();
            scripts.borrow_mut().push((saved.address.clone(), script));
            client
        });
        let opened = self.opened.clone();
        let open = Rc::new(
            move |saved: SavedRemote, seat: SeatRequest, _: &mut gpui::App| {
                opened.borrow_mut().push((saved.address, seat));
            },
        );
        let notifier: Rc<dyn Notifier> = self.notifier.clone();
        let ownership = Ownership::new(fs, Some(PathBuf::from(LOCK)));
        let links = app.new(|_| WorkerLinks::new(store, dial, open, notifier, ownership));
        let handle = links.clone();
        app.update(|cx| cx.set_global(WorkerLinksHandle(handle)));
        links
    }

    /// The `n`th link dialled.
    fn script(&self, n: usize) -> std::cell::RefMut<'_, Script> {
        std::cell::RefMut::map(self.scripts.borrow_mut(), |s| &mut s[n].1)
    }

    fn dialled(&self) -> Vec<String> {
        self.scripts
            .borrow()
            .iter()
            .map(|(a, _)| a.clone())
            .collect()
    }

    fn posted(&self) -> Vec<(String, String, NotificationSound)> {
        self.notifier
            .posted()
            .into_iter()
            .map(|n| (n.title, n.body, n.sound))
            .collect()
    }
}

fn turn(links: &Entity<WorkerLinks>, app: &mut TestAppContext) {
    links.update(app, |l, cx| l.turn(cx));
}

fn rows(links: &Entity<WorkerLinks>, app: &mut TestAppContext) -> Vec<WorkerRow> {
    links.read_with(app, |l, _| l.rows())
}

fn saved(label: &str, address: &str, kind: Option<HostKind>) -> SavedRemote {
    SavedRemote {
        label: label.to_string(),
        address: address.to_string(),
        token: format!("token-{label}"),
        last_seen: Some(1_700_000_000),
        viewer_id: None,
        host_version: Some("0.0.1".to_string()),
        warned_unencrypted: false,
        ssh_target: None,
        host_kind: kind,
    }
}

fn session(id: &str, bucket: StatusBucket) -> SessionView {
    SessionView {
        session_id: TabId(id.to_string()),
        project_id: "/home/u/flightdeck/app".into(),
        name: id.to_string(),
        agent: "claude".to_string(),
        agent_display_name: "Claude Code".to_string(),
        phase: SessionPhase::Ready,
        status: SessionStatus {
            interpreted: InterpretedStatus::Working,
            manual: None,
            bucket,
            running_time_secs: 30,
        },
        git: GitBar::default(),
        terminals: Vec::new(),
        lifecycle_reporting: true,
        recovered: false,
        attached_existing_branch: false,
        worktree_path: format!("/home/u/flightdeck/app/.flightdeck/worktrees/{id}"),
    }
}

fn health(version: &str) -> Health {
    Health {
        sampled_at_ms: 0,
        uptime_secs: 3_600,
        version: version.to_string(),
        load_avg_1m_centi: Some(42),
        cpu_count: 8,
        mem_total_bytes: None,
        mem_available_bytes: None,
        disk_free_home_bytes: Some(18_000_000_000),
        disk_free_projects_root_bytes: None,
        agents_total: 3,
        agents_running: 2,
    }
}

/// What a worker sends a background link: three sessions, two of which need
/// the person, two unread host messages.
fn worker_snapshot(version: &str) -> Snapshot {
    Snapshot {
        protocol_version: PROTOCOL_VERSION,
        host_version: version.to_string(),
        server_time_ms: 0,
        viewer_id: "viewer-bg".into(),
        seat: Seat::Observing,
        seats: Vec::new(),
        last_input_seq: 0,
        projects: vec![ProjectView {
            project_id: "/home/u/flightdeck/app".into(),
            name: "app".to_string(),
            root: "/home/u/flightdeck/app".to_string(),
            base_branch: "main".to_string(),
            dot: Some(StatusBucket::Waiting),
            sessions: vec![
                session("s1", StatusBucket::Waiting),
                session("s2", StatusBucket::InProgress),
                session("s3", StatusBucket::Error),
            ],
        }],
        selection: Selection {
            project_id: None,
            session_id: None,
            terminal_id: None,
            split_view: false,
        },
        geometry: Geometry {
            cols: 200,
            rows: 50,
        },
        replay_capacity_bytes: 65_536,
        activity: Vec::new(),
        dialog: None,
        commands: Vec::new(),
        help: None,
        about: None,
        update: None,
        sidebar_position: AgentTabPosition::default(),
        host_machine: None,
        host_kind: Some(HostKind::Worker),
        worker: Some(WorkerSnapshot {
            home: "/home/u".to_string(),
            shell: None,
            health: Some(health(version)),
            status: WorkerStatusView {
                listening: vec!["100.64.0.2:7420".to_string()],
                advertised: Some(BOX.to_string()),
                not_listening_reason: None,
                unread_messages: 2,
                projects_root: "/home/u/flightdeck".to_string(),
            },
            connections: ConnectionsView::default(),
            host_messages: Vec::new(),
        }),
    }
}

fn raised(title: &str) -> LinkEvent {
    LinkEvent::Delta(Delta::Notification(NotificationView {
        at_ms: 0,
        title: title.to_string(),
        body: "Claude Code needs your input".to_string(),
        sound: NotificationSoundView::InputRequired,
    }))
}

/// A rig with one saved worker (`box`), linked and live on a worker
/// snapshot of this app's own core version.
fn live_worker(app: &mut TestAppContext) -> (Rig, Entity<WorkerLinks>) {
    let rig = Rig::new();
    rig.save(vec![saved("box", BOX, Some(HostKind::Worker))]);
    let links = rig.links(app);
    turn(&links, app);
    rig.script(0).snapshot(worker_snapshot(flightdeck::VERSION));
    turn(&links, app);
    (rig, links)
}

#[test]
fn a_background_link_watches_with_a_minute_backoff() {
    let config = link_config(&saved("box", BOX, Some(HostKind::Worker)));
    assert_eq!(config.address, BOX);
    assert_eq!(config.connection, ConnectionKind::Background);
    assert_eq!(config.seat, SeatRequest::Observe);
    assert_eq!(config.retry_cap_ms, Some(RETRY_CAP_MS));
    assert_eq!(RETRY_CAP_MS, 60_000);
    assert!(
        config.user_agent.starts_with("FlightDeck Desktop/") && config.user_agent.ends_with(')'),
        "labelled with this machine's name: {}",
        config.user_agent
    );
}

#[gpui::test]
fn only_saved_workers_get_a_link(app: &mut TestAppContext) {
    let rig = Rig::new();
    rig.save(vec![
        saved("box", BOX, Some(HostKind::Worker)),
        saved("studio", "studio.local:7420", Some(HostKind::Tui)),
        saved("old", "old.local:7420", None),
        saved("gpu", "gpu.ts.net:7420", Some(HostKind::Worker)),
    ]);
    let links = rig.links(app);
    assert!(
        rows(&links, app).is_empty(),
        "nothing dialled before a turn"
    );
    turn(&links, app);
    assert_eq!(rig.dialled(), [BOX, "gpu.ts.net:7420"]);
    let rows = rows(&links, app);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].label, "box");
    assert_eq!(rows[0].state, WorkerState::Connecting);
    assert_eq!(rows[1].label, "gpu");
}

#[gpui::test]
fn rows_reflect_a_scripted_worker_snapshot(app: &mut TestAppContext) {
    let (_rig, links) = live_worker(app);
    let rows = rows(&links, app);
    assert_eq!(
        rows,
        [WorkerRow {
            label: "box".to_string(),
            address: BOX.to_string(),
            state: WorkerState::Online,
            needs_you: 2,
            version: Some(flightdeck::VERSION.to_string()),
            skew: false,
            health: Some(health(flightdeck::VERSION)),
            unread: 2,
        }]
    );
    assert_eq!(
        super::view::detail_line(&rows[0], 0),
        "load 0.42 · 18 GB free · 3 agents"
    );
}

#[gpui::test]
fn link_states_become_row_states(app: &mut TestAppContext) {
    let (rig, links) = live_worker(app);
    let state = |app: &mut TestAppContext| rows(&links, app)[0].state.clone();

    rig.script(0)
        .send(LinkEvent::State(LinkState::Reconnecting {
            attempt: 1,
            retry_in_ms: 250,
        }));
    turn(&links, app);
    let WorkerState::Offline {
        last_seen: Some(seen),
    } = state(app)
    else {
        panic!("offline, seen just now: {:?}", state(app));
    };
    assert!(seen > 1_700_000_000, "the live link's time, not the file's");
    let row = &rows(&links, app)[0];
    assert_eq!(
        (row.needs_you, row.health.is_none()),
        (0, true),
        "an offline worker's last numbers are not shown as live"
    );

    for (end, expected) in [
        (LinkEnd::Unauthorized, WorkerState::NotPaired),
        (LinkEnd::Revoked, WorkerState::Revoked),
        (
            LinkEnd::VersionMismatch("Update FlightDeck on the worker.".into()),
            WorkerState::UpdateNeeded("Update FlightDeck on the worker.".into()),
        ),
    ] {
        rig.script(0).send(LinkEvent::State(LinkState::Ended(end)));
        turn(&links, app);
        assert_eq!(state(app), expected);
    }
}

#[gpui::test]
fn a_never_reached_worker_is_offline_since_its_saved_time(app: &mut TestAppContext) {
    let rig = Rig::new();
    rig.save(vec![saved("box", BOX, Some(HostKind::Worker))]);
    let links = rig.links(app);
    turn(&links, app);
    rig.script(0)
        .send(LinkEvent::State(LinkState::Reconnecting {
            attempt: 1,
            retry_in_ms: 250,
        }));
    turn(&links, app);
    let row = &rows(&links, app)[0];
    assert_eq!(
        row.state,
        WorkerState::Offline {
            last_seen: Some(1_700_000_000)
        }
    );
    assert_eq!(row.version.as_deref(), Some("0.0.1"), "the saved version");
    assert!(row.skew);
}

#[gpui::test]
fn forgotten_workers_are_dropped_and_new_ones_dialled(app: &mut TestAppContext) {
    let (rig, links) = live_worker(app);
    rig.save(vec![saved(
        "gpu",
        "gpu.ts.net:7420",
        Some(HostKind::Worker),
    )]);
    links.update(app, |l, _| l.reload(Instant::now()));
    assert_eq!(rig.dialled(), [BOX, "gpu.ts.net:7420"]);
    let rows = rows(&links, app);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].label, "gpu");

    // Re-paired: a new token means a new link.
    let mut repaired = saved("gpu", "gpu.ts.net:7420", Some(HostKind::Worker));
    repaired.token = "fresh".to_string();
    rig.save(vec![repaired]);
    links.update(app, |l, _| l.reload(Instant::now()));
    assert_eq!(rig.dialled().len(), 3);

    // A rename keeps the link.
    let mut renamed = saved("gpu box", "gpu.ts.net:7420", Some(HostKind::Worker));
    renamed.token = "fresh".to_string();
    rig.save(vec![renamed]);
    links.update(app, |l, _| l.reload(Instant::now()));
    assert_eq!(rig.dialled().len(), 3);
    assert_eq!(rows_label(&links, app), ["gpu box"]);
}

fn rows_label(links: &Entity<WorkerLinks>, app: &mut TestAppContext) -> Vec<String> {
    rows(links, app).into_iter().map(|r| r.label).collect()
}

#[gpui::test]
fn an_ended_link_that_may_recover_is_dialled_again(app: &mut TestAppContext) {
    let (rig, links) = live_worker(app);
    rig.script(0).send(LinkEvent::State(LinkState::Ended(
        LinkEnd::VersionMismatch("update".into()),
    )));
    turn(&links, app);
    let soon = Instant::now();
    links.update(app, |l, _| l.reload(soon));
    assert_eq!(rig.dialled().len(), 1, "not before a minute has passed");
    let later = soon + std::time::Duration::from_millis(RETRY_CAP_MS);
    links.update(app, |l, _| l.reload(later));
    assert_eq!(rig.dialled().len(), 2);
    assert_eq!(
        rows(&links, app)[0].state,
        WorkerState::UpdateNeeded("update".into()),
        "the last word stands while it connects again"
    );

    // A refused token is not retried: only pairing again fixes it.
    rig.script(1)
        .send(LinkEvent::State(LinkState::Ended(LinkEnd::Revoked)));
    turn(&links, app);
    let much_later = later + std::time::Duration::from_secs(600);
    links.update(app, |l, _| l.reload(much_later));
    assert_eq!(rig.dialled().len(), 2);
}

#[gpui::test]
fn a_notification_is_posted_while_no_window_is_open(app: &mut TestAppContext) {
    let (rig, links) = live_worker(app);
    assert!(links.read_with(app, |l, _| l.owns_notifications()));
    rig.script(0).send(raised("fix-login"));
    turn(&links, app);
    assert_eq!(
        rig.posted(),
        [(
            "box: fix-login".to_string(),
            "Claude Code needs your input".to_string(),
            NotificationSound::InputRequired
        )]
    );
    turn(&links, app);
    assert_eq!(rig.posted().len(), 1, "posted once");
}

#[gpui::test]
fn the_open_window_notifies_instead(app: &mut TestAppContext) {
    let (rig, links) = live_worker(app);
    app.update(|cx| registry::opened(" BOX.ts.net:7420", cx));
    rig.script(0).send(raised("fix-login"));
    turn(&links, app);
    assert!(rig.posted().is_empty(), "the window posts it");

    app.update(|cx| registry::closed(BOX, cx));
    rig.script(0).send(raised("search"));
    turn(&links, app);
    assert_eq!(rig.posted().len(), 1, "only what arrived after it closed");
    assert_eq!(rig.posted()[0].0, "box: search");
}

#[gpui::test]
fn a_window_from_this_machine_elsewhere_notifies_instead(app: &mut TestAppContext) {
    let (rig, links) = live_worker(app);
    let window = |same_device| {
        LinkEvent::Delta(Delta::Connections(ConnectionsView {
            window: Some(WindowInfo {
                label: "studio".to_string(),
                address: None,
                since_ms: 0,
                same_device,
            }),
            background_links: 2,
        }))
    };
    rig.script(0).send(window(true));
    rig.script(0).send(raised("fix-login"));
    turn(&links, app);
    assert!(
        rig.posted().is_empty(),
        "this Desktop's other window posts it"
    );

    // Another machine's window does not stop this one hearing about it.
    rig.script(0).send(window(false));
    rig.script(0).send(raised("search"));
    turn(&links, app);
    assert_eq!(rig.posted().len(), 1);
}

#[gpui::test]
fn a_process_without_the_lock_does_not_post(app: &mut TestAppContext) {
    let rig = Rig::new();
    // Another Desktop process on this machine owns the notifications.
    let other = rig.fs.try_lock_exclusive(Path::new(LOCK)).unwrap();
    assert!(other.is_some());
    rig.save(vec![saved("box", BOX, Some(HostKind::Worker))]);
    let links = rig.links(app);
    turn(&links, app);
    rig.script(0).snapshot(worker_snapshot(flightdeck::VERSION));
    rig.script(0).send(raised("fix-login"));
    turn(&links, app);
    assert!(!links.read_with(app, |l, _| l.owns_notifications()));
    assert!(rig.posted().is_empty());
    // What it heard meanwhile is not saved up for later.
    drop(other);
    links.update(app, |l, _| l.retry_ownership_now());
    turn(&links, app);
    assert!(links.read_with(app, |l, _| l.owns_notifications()));
    assert!(rig.posted().is_empty());
    rig.script(0).send(raised("search"));
    turn(&links, app);
    assert_eq!(rig.posted().len(), 1, "the new owner posts");
}

#[test]
fn ownership_is_retried_every_thirty_seconds() {
    let fs = Rc::new(FakeFs::new());
    let held = fs.try_lock_exclusive(Path::new(LOCK)).unwrap();
    let mut ownership = Ownership::new(fs.clone(), Some(PathBuf::from(LOCK)));
    let start = Instant::now();
    assert!(!ownership.refresh(start));
    drop(held);
    assert!(
        !ownership.refresh(start + std::time::Duration::from_secs(29)),
        "not tried again yet"
    );
    assert!(ownership.refresh(start + std::time::Duration::from_secs(30)));
    assert!(ownership.owns());
    // Without a home directory there is no lock to own.
    let mut homeless = Ownership::new(fs, None);
    assert!(!homeless.refresh(start));
}

// -- the launcher -----------------------------------------------------------

fn launcher<'a>(
    app: &'a mut TestAppContext,
    version: &str,
) -> (Rig, Entity<WorkerLinks>, &'a mut VisualTestContext) {
    app.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        flightdeck_desktop::keys::register(cx, crate::commands::keymap());
        flightdeck_desktop::overlays::register(cx, crate::commands::keymap());
    });
    let rig = Rig::new();
    rig.save(vec![saved("box", BOX, Some(HostKind::Worker))]);
    let links = rig.links(app);
    turn(&links, app);
    rig.script(0).snapshot(worker_snapshot(version));
    let opener: Opener = Rc::new(|_| Err("no opener in this test".to_string()));
    let (_root, cx) = app.add_window_view(move |window, cx| {
        let root = cx.new(|cx| AppRoot::new(None, opener, Vec::new(), None, false, cx));
        Root::new(root, window, cx)
    });
    cx.run_until_parked();
    links.update(cx, |l, cx| l.turn(cx));
    cx.run_until_parked();
    (rig, links, cx)
}

#[gpui::test]
fn the_launcher_lists_workers_and_opens_one_on_click(app: &mut TestAppContext) {
    let (rig, _links, cx) = launcher(app, flightdeck::VERSION);
    assert!(cx.debug_bounds("workers").is_some());
    assert!(cx.debug_bounds("worker-0").is_some());
    assert!(cx.debug_bounds("worker-0-state").is_some());
    assert!(cx.debug_bounds("worker-0-needs-you").is_some());
    assert!(cx.debug_bounds("worker-0-unread").is_some());
    assert!(cx.debug_bounds("worker-0-skew").is_none(), "same version");
    assert!(cx.debug_bounds("worker-1").is_none());

    let bounds = cx.debug_bounds("worker-0").unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        *rig.opened.borrow(),
        [(BOX.to_string(), SeatRequest::Write)],
        "its remote window, with the writing seat"
    );
}

#[gpui::test]
fn a_worker_on_another_version_shows_the_skew_badge(app: &mut TestAppContext) {
    let (_rig, links, cx) = launcher(app, "0.0.1-other");
    assert!(links.read_with(cx, |l, _| l.rows()[0].skew));
    assert!(cx.debug_bounds("worker-0-skew").is_some());
}

#[gpui::test]
fn no_saved_workers_means_no_section(app: &mut TestAppContext) {
    app.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
    });
    let rig = Rig::new();
    let links = rig.links(app);
    turn(&links, app);
    let opener: Opener = Rc::new(|_| Err("no opener in this test".to_string()));
    let (_root, cx) = app.add_window_view(move |window, cx| {
        let root = cx.new(|cx| AppRoot::new(None, opener, Vec::new(), None, false, cx));
        Root::new(root, window, cx)
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("launcher").is_some());
    assert!(cx.debug_bounds("workers").is_none());
}

#[gpui::test]
fn the_launcher_shows_a_running_workers_refusal(app: &mut TestAppContext) {
    app.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
    });
    let refusal = flightdeck::worker::lock::LOCAL_HOST_REFUSAL;
    let opener: Opener = Rc::new(move |_| Err(refusal.to_string()));
    let slot: Rc<RefCell<Option<Entity<AppRoot>>>> = Rc::default();
    let for_window = slot.clone();
    let (_root, cx) = app.add_window_view(move |window, cx| {
        let root = cx.new(|cx| AppRoot::new(None, opener, Vec::new(), None, false, cx));
        *for_window.borrow_mut() = Some(root.clone());
        Root::new(root, window, cx)
    });
    let root = slot.borrow().clone().unwrap();
    root.update(cx, |r, cx| r.open_folder(PathBuf::from("/work/app"), cx));
    cx.run_until_parked();
    assert!(root.read_with(cx, |r, _| r.is_launcher()));
    assert_eq!(
        root.read_with(cx, |r, _| r.error().map(str::to_string)),
        Some(refusal.to_string())
    );
}
