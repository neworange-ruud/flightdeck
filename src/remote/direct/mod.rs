//! The direct link: the phone protocol served by this Mac over Tailscale, with
//! a Mac-held outbox in place of the relay's queue (spec §5.10,
//! remote-control-inlb).
//!
//! ```text
//!   bridge ──SendEnvelope──▶ [router] ──relay-mode──▶ relay client ──▶ relay
//!                               │
//!                               └─direct-mode──▶ outbox (per pairing, on disk)
//!                                                   │ replay from resume / live
//!   phone ◀════ ws://<tailscale host>:<port>/remote/v1 ════ direct server
//!         ════ envelope / ack / resume / ping / revoke ═══▶  │
//!                                                   RemoteInbound ──▶ app/bridge
//! ```
//!
//! ## Why a dedicated listener, not the embedded web server
//!
//! The web server (`src/web/`) is the browser surface: it starts on demand,
//! binds loopback or an explicitly typed network address, authenticates browsers
//! with access codes and cookies, and speaks a different protocol (its module
//! docs in `src/web/protocol.rs` explain why the phone protocol is not reused
//! there). The direct link must run whenever Remote + `direct_enabled` are on —
//! independent of whether anyone opened the web interface — bind **only**
//! Tailscale addresses, and authenticate the phone by its pairing identity key.
//! Sharing a listener would couple two opt-ins and two auth models for no gain,
//! so it is its own `tokio` listener on the one shared runtime
//! ([`crate::remote::runtime`]), exactly like the relay client.
//!
//! ## How a pairing's envelopes are routed
//!
//! The bridge seals and numbers every outbound envelope as before and hands it
//! to the relay client's channel. [`OutboundRouter`] sits on that channel's
//! forwarding thread (see `RemoteHandle::start_with_options`): an envelope for a
//! **direct-mode** pairing goes into that pairing's [`outbox`] and never reaches
//! the relay; everything else passes through untouched, so relay-mode pairings
//! behave exactly as before. The relay client additionally ignores data-plane
//! frames (presence, envelopes, acks) the relay might send for a direct pairing,
//! and never sends one a `resume` — on a direct pairing the relay only carries
//! pairing bootstrap, push-token registration, revoke and pushes.
//!
//! ## Pairing state stays single-writer
//!
//! `remote.json` is owned by the relay client, which keeps it in memory and
//! saves it whole. The direct link never writes it behind the client's back:
//! [`DirectLink::wrap_store`] wraps the client's store, so every save the client
//! makes also refreshes the set of direct pairings this link authenticates
//! (a claim adds one, a relay revoke or an unpair removes it), and a phone's
//! revoke over the direct link tombstones the pairing in that wrapper so it is
//! filtered out of every save and load from then on — "exactly as a relay
//! revoke": the record is gone, the outbox is deleted, the app is told.
//! The direct link's own per-pairing state — the outbox, its seq high-water mark
//! and the phone→desktop dedup cursor — lives in its own files under
//! `~/.flightdeck/remote-direct/` (see [`outbox`]).

pub mod net;
pub mod outbox;
mod server;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use flightdeck_remote_protocol::relay::direct_auth_message;
use flightdeck_remote_protocol::{
    DeviceId, DirectEndpoint, EncryptedEnvelope, PairingId, PresenceState, Role,
};
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use tokio::sync::watch;

use crate::contracts::{FileSystem, RemoteConfig};
use crate::remote::client::RemoteStore;
use crate::remote::state::RemoteState;
use crate::remote::{RemoteInbound, RemoteOutbound};

use net::{AddressSource, TailscaleProbe};
use outbox::{OutboxFile, OutboxLimits, ResumeOutcome};

/// How often dirty outboxes are written (see [`outbox`] on why writes are
/// debounced and how seq continuity survives a crash anyway).
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(500);

/// The directory under `~/.flightdeck/` holding the per-pairing outboxes.
pub const STATE_DIR_NAME: &str = "remote-direct";

/// The endpoint new pairing offers advertise, shared between the direct link
/// (which fills it once a listener is bound) and the relay client (which reads
/// it when it builds a `pairing_offer`). `None` while nothing is bound, so a
/// pairing made then is relay mode rather than pointing at a dead address.
pub type DirectAdvert = Arc<RwLock<Option<DirectEndpoint>>>;

/// Read the advertised endpoint (a poisoned lock reads as "none").
pub fn advertised(advert: &DirectAdvert) -> Option<DirectEndpoint> {
    advert.read().ok().and_then(|g| g.clone())
}

/// Splits the app's outbound traffic between the relay client and the direct
/// link. Runs on the relay client's forwarding thread, so it must not block
/// for long.
pub trait OutboundRouter: Send + Sync {
    /// Take `msg` if it belongs to the direct link (returning `None`), or hand
    /// it back for the relay client. May also act on a message and still pass
    /// it on (an unpair must reach both).
    fn route(&self, msg: RemoteOutbound) -> Option<RemoteOutbound>;
}

/// Where the direct link listens.
pub enum BindPlan {
    /// No listener. Used when direct pairings exist but `direct_enabled` was
    /// switched off: their envelopes must still never go to the relay, so they
    /// collect (bounded) in the outbox until the link is switched back on.
    None,
    /// Exactly these addresses (tests bind loopback, port 0). The advertised
    /// host is the first address's IP.
    Fixed(Vec<SocketAddr>),
    /// This machine's Tailscale addresses on `port`, re-checked every
    /// [`server::REBIND_INTERVAL`] so Tailscale coming up (or changing address)
    /// after launch is picked up without a restart.
    Tailscale {
        /// The configured `direct_port`.
        port: u16,
        /// `[remote] direct_host` (empty = discover).
        host_override: String,
        /// Interface enumeration seam.
        source: Arc<dyn AddressSource>,
        /// MagicDNS seam.
        probe: Arc<dyn TailscaleProbe>,
    },
}

/// Everything [`DirectLink::start`] needs.
pub struct DirectOptions {
    /// Root of the per-pairing outbox directories.
    pub state_dir: PathBuf,
    /// Filesystem seam for the outboxes.
    pub fs: Arc<dyn FileSystem + Send + Sync>,
    /// Where to listen.
    pub bind: BindPlan,
    /// Outbox bounds.
    pub limits: OutboxLimits,
    /// The name announced in `machine_name` frames; `None` resolves this
    /// machine's name per connection, as the relay client does.
    pub machine_name: Option<String>,
}

impl DirectOptions {
    /// The production options for `cfg`, with the outboxes beside
    /// `remote_state_path` (`~/.flightdeck/remote.json` →
    /// `~/.flightdeck/remote-direct/`). Listens on Tailscale only when
    /// `direct_enabled`; otherwise [`BindPlan::None`].
    pub fn for_config(cfg: &RemoteConfig, remote_state_path: &Path) -> Self {
        let bind = if cfg.direct_enabled {
            BindPlan::Tailscale {
                port: cfg.direct_port,
                host_override: cfg.direct_host.clone(),
                source: Arc::new(net::RealAddressSource),
                probe: Arc::new(net::CliTailscaleProbe),
            }
        } else {
            BindPlan::None
        };
        DirectOptions {
            state_dir: remote_state_path.with_file_name(STATE_DIR_NAME),
            fs: Arc::new(crate::contracts::RealFs),
            bind,
            limits: OutboxLimits::default(),
            machine_name: None,
        }
    }
}

/// Whether a direct link is needed at all: the user switched it on, or direct
/// pairings exist whose envelopes must stay off the relay.
pub fn needed(cfg: &RemoteConfig, state: &RemoteState) -> bool {
    cfg.direct_enabled || state.pairings.iter().any(|p| p.is_direct())
}

/// A direct-mode pairing this link authenticates.
#[derive(Debug, Clone)]
struct DirectPeer {
    /// The phone's device id, as recorded at pairing time.
    peer_device_id: String,
    /// The phone's identity key (X9.63, 65 bytes).
    peer_public_key: Vec<u8>,
}

/// The app-bound channel, shareable across tasks (`Sender` is not `Sync`; see
/// the same wrapper in `client.rs`).
struct InboundTx(Mutex<Sender<RemoteInbound>>);

impl InboundTx {
    fn send(&self, msg: RemoteInbound) {
        if let Ok(tx) = self.0.lock() {
            let _ = tx.send(msg);
        }
    }
}

/// Mutable link state behind one lock. Critical sections are short; the only
/// I/O under it is an outbox write.
#[derive(Default)]
struct Inner {
    /// Authenticable direct pairings, by pairing id.
    peers: HashMap<String, DirectPeer>,
    /// Every pairing id recorded as direct mode — including one offered but
    /// not yet claimed — so the router never lets its envelopes reach the relay.
    direct_ids: HashSet<String>,
    /// One outbox per authenticable pairing.
    outboxes: HashMap<String, OutboxFile>,
    /// Pairings revoked or unpaired through the direct link this process.
    tombstones: HashSet<String>,
    /// Which session currently serves each pairing (newest wins).
    live: HashMap<String, u64>,
    /// Session id allocator.
    next_session: u64,
}

/// The persistence side of [`DirectAwareStore`], serialized by its own lock.
#[derive(Default)]
struct Persist {
    /// The client's real store, once wrapped.
    store: Option<Arc<dyn RemoteStore>>,
    /// The last state the client saved (filtered), to rewrite on a revoke.
    last: Option<RemoteState>,
}

/// State shared by the router, the store wrapper, the listeners and sessions.
pub(crate) struct Shared {
    fs: Arc<dyn FileSystem + Send + Sync>,
    state_dir: PathBuf,
    limits: OutboxLimits,
    inbound: InboundTx,
    inner: Mutex<Inner>,
    persist: Mutex<Persist>,
    /// Bumped after every outbox push: wakes sessions for live delivery.
    outbox_tick: watch::Sender<u64>,
    /// Bumped whenever a pairing is added, removed, or changes session: wakes
    /// sessions to re-check what they may still serve.
    registry_tick: watch::Sender<u64>,
    advert: DirectAdvert,
    machine_name: Option<String>,
    /// Bound listener addresses (diagnostics and tests).
    bound: Mutex<Vec<SocketAddr>>,
}

fn bump(tick: &watch::Sender<u64>) {
    tick.send_modify(|n| *n = n.wrapping_add(1));
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves plain data behind; keep serving.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Reconcile the authenticable set with the client's pairing records.
    /// Returns the pairing ids newly added.
    fn sync_from_state(&self, state: &RemoteState) -> Vec<String> {
        let mut added = Vec::new();
        let mut changed = false;
        {
            let mut inner = self.lock();
            let tomb = inner.tombstones.clone();
            inner.direct_ids = state
                .pairings
                .iter()
                .filter(|p| p.is_direct() && !tomb.contains(&p.pairing_id))
                .map(|p| p.pairing_id.clone())
                .collect();
            let wanted: HashMap<String, DirectPeer> = state
                .pairings
                .iter()
                .filter(|p| p.is_direct() && p.established && !tomb.contains(&p.pairing_id))
                .filter_map(|p| {
                    let peer_device_id = p.peer_device_id.clone()?;
                    let key = STANDARD.decode(p.peer_device_public_key.as_deref()?).ok()?;
                    VerifyingKey::from_sec1_bytes(&key).ok()?;
                    Some((
                        p.pairing_id.clone(),
                        DirectPeer {
                            peer_device_id,
                            peer_public_key: key,
                        },
                    ))
                })
                .collect();
            let gone: Vec<String> = inner
                .peers
                .keys()
                .filter(|k| !wanted.contains_key(*k))
                .cloned()
                .collect();
            for pid in gone {
                inner.peers.remove(&pid);
                if let Some(ob) = inner.outboxes.remove(&pid) {
                    let _ = ob.delete();
                }
                changed = true;
                crate::remote::debuglog::log(&format!("direct UNREGISTER pairing={pid}"));
            }
            for (pid, peer) in wanted {
                if !inner.peers.contains_key(&pid) {
                    let ob = OutboxFile::open(self.fs.clone(), &self.state_dir, &pid, self.limits);
                    inner.outboxes.insert(pid.clone(), ob);
                    added.push(pid.clone());
                    changed = true;
                    crate::remote::debuglog::log(&format!("direct REGISTER pairing={pid}"));
                }
                inner.peers.insert(pid, peer);
            }
        }
        if changed {
            bump(&self.registry_tick);
        }
        added
    }

    /// Drop a pairing from the link: refuse it from now on, delete its outbox,
    /// tombstone it out of `remote.json`, and close any session serving it.
    fn forget(&self, pairing_id: &str) {
        {
            let mut inner = self.lock();
            inner.peers.remove(pairing_id);
            inner.live.remove(pairing_id);
            if let Some(ob) = inner.outboxes.remove(pairing_id) {
                let _ = ob.delete();
            } else {
                // Never registered this process (e.g. offered, not claimed):
                // still clear anything a previous process left on disk.
                let ob =
                    OutboxFile::open(self.fs.clone(), &self.state_dir, pairing_id, self.limits);
                let _ = ob.delete();
            }
            inner.tombstones.insert(pairing_id.to_string());
        }
        bump(&self.registry_tick);
        // Rewrite remote.json without it (the client's in-memory copy is
        // filtered on its next save; see the module docs).
        let mut persist = self.persist.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(store) = persist.store.clone() {
            let mut state = persist.last.clone().unwrap_or_else(|| store.load());
            state.pairings.retain(|p| p.pairing_id != pairing_id);
            store.save(&state);
            persist.last = Some(state);
        }
        crate::remote::debuglog::log(&format!("direct FORGET pairing={pairing_id}"));
    }

    fn is_direct(&self, pairing_id: &str) -> bool {
        let inner = self.lock();
        inner.direct_ids.contains(pairing_id) || inner.tombstones.contains(pairing_id)
    }

    fn filter_tombstones(&self, state: &mut RemoteState) {
        let inner = self.lock();
        if !inner.tombstones.is_empty() {
            state
                .pairings
                .retain(|p| !inner.tombstones.contains(&p.pairing_id));
        }
    }

    /// Queue an outbound envelope for a direct pairing and wake live sessions.
    fn push(&self, env: EncryptedEnvelope) {
        let pid = env.pairing_id.as_str().to_string();
        let seq = env.seq;
        let pushed = {
            let mut inner = self.lock();
            match inner.outboxes.get_mut(&pid) {
                Some(ob) => {
                    let out = ob.push(env);
                    if out.restarted || out.dropped > 0 {
                        crate::remote::debuglog::log(&format!(
                            "direct OUTBOX pairing={pid} seq={seq} restarted={} dropped={}",
                            out.restarted, out.dropped
                        ));
                    }
                    true
                }
                None => false,
            }
        };
        if pushed {
            bump(&self.outbox_tick);
        } else {
            // A direct pairing with no outbox is one not yet claimed or already
            // revoked: dropping is right, and the relay must never see it.
            crate::remote::debuglog::log(&format!(
                "direct DROP envelope pairing={pid} seq={seq} (no outbox)"
            ));
        }
    }

    /// Verify a phone's direct `auth_response` (spec §5.10): every requested
    /// pairing must be a live direct pairing whose peer is `device_id`, and the
    /// signature over `direct_auth_message(nonce)` must verify against the
    /// identity key recorded for each.
    fn authenticate(
        &self,
        device_id: &str,
        pairing_ids: &[PairingId],
        nonce: &[u8],
        signature_b64: &str,
    ) -> Result<(), &'static str> {
        if pairing_ids.is_empty() {
            return Err("no pairings requested");
        }
        let sig_bytes = STANDARD
            .decode(signature_b64)
            .map_err(|_| "malformed signature")?;
        let signature = Signature::from_slice(&sig_bytes).map_err(|_| "malformed signature")?;
        let message = direct_auth_message(nonce);
        let inner = self.lock();
        for pid in pairing_ids {
            let peer = inner
                .peers
                .get(pid.as_str())
                .ok_or("unknown or revoked pairing")?;
            if peer.peer_device_id != device_id {
                return Err("pairing belongs to another device");
            }
            let key = VerifyingKey::from_sec1_bytes(&peer.peer_public_key)
                .map_err(|_| "unusable device key")?;
            key.verify(&message, &signature)
                .map_err(|_| "signature does not verify")?;
        }
        Ok(())
    }

    /// Claim `pairings` for session `session` (superseding any older session)
    /// and tell the app the phone is here.
    fn attach(&self, session: u64, pairings: &[PairingId], device_id: &str) {
        let mut cursors = Vec::new();
        {
            let mut inner = self.lock();
            for pid in pairings {
                inner.live.insert(pid.as_str().to_string(), session);
                let cursor = inner
                    .outboxes
                    .get(pid.as_str())
                    .map(|o| o.outbox().ack_cursor())
                    .unwrap_or(0);
                cursors.push((pid.clone(), cursor));
            }
        }
        bump(&self.registry_tick);
        for (pid, cursor) in cursors {
            self.inbound.send(RemoteInbound::Paired {
                pairing_id: pid.clone(),
                peer_device_id: Some(DeviceId::new(device_id)),
            });
            self.inbound.send(RemoteInbound::Presence {
                pairing_id: pid.clone(),
                peer: Role::Phone,
                state: PresenceState::Connected,
            });
            // Mirrors the relay echoing the stored ack cursor after auth: it is
            // what tells the bridge acks flow on this link (remote-control-5qu).
            self.inbound.send(RemoteInbound::PeerAck {
                pairing_id: pid,
                cursor,
            });
        }
    }

    /// Release whatever of `pairings` session `session` still serves and tell
    /// the app the phone left. A pairing a newer session took over is left
    /// alone (that session announced itself; a "disconnected" would libel it).
    fn detach(&self, session: u64, pairings: &[PairingId]) {
        let mut left = Vec::new();
        {
            let mut inner = self.lock();
            for pid in pairings {
                if inner.live.get(pid.as_str()) == Some(&session) {
                    inner.live.remove(pid.as_str());
                    left.push(pid.clone());
                }
            }
        }
        for pid in left {
            self.inbound.send(RemoteInbound::Presence {
                pairing_id: pid,
                peer: Role::Phone,
                state: PresenceState::Disconnected,
            });
        }
    }

    /// Whether session `session` may still serve `pairing_id`: it is still
    /// registered and no newer session took it over. `Err(true)` = revoked or
    /// unpaired, `Err(false)` = superseded.
    fn still_serves(&self, session: u64, pairing_id: &str) -> Result<(), bool> {
        let inner = self.lock();
        if !inner.peers.contains_key(pairing_id) {
            return Err(true);
        }
        match inner.live.get(pairing_id) {
            Some(s) if *s == session => Ok(()),
            _ => Err(false),
        }
    }

    fn new_session_id(&self) -> u64 {
        let mut inner = self.lock();
        inner.next_session += 1;
        inner.next_session
    }

    fn resume(&self, pairing_id: &str, from_seq: u64) -> ResumeOutcome {
        let mut inner = self.lock();
        match inner.outboxes.get_mut(pairing_id) {
            Some(ob) => ob.resume(from_seq),
            None => ResumeOutcome::Replay(Vec::new()),
        }
    }

    fn after(&self, pairing_id: &str, seq: u64) -> Vec<EncryptedEnvelope> {
        let inner = self.lock();
        inner
            .outboxes
            .get(pairing_id)
            .map(|o| o.outbox().after(seq))
            .unwrap_or_default()
    }

    fn ack(&self, pairing_id: &str, cursor: u64) {
        let mut inner = self.lock();
        if let Some(ob) = inner.outboxes.get_mut(pairing_id) {
            ob.ack(cursor);
        }
    }

    /// Phone→desktop dedup (§6.4). `Ok(seq)` = new, deliver it; `Err(cursor)` =
    /// a duplicate, re-ack the current cursor.
    fn accept_inbound(&self, pairing_id: &str, seq: u64) -> Result<u64, u64> {
        let mut inner = self.lock();
        let Some(ob) = inner.outboxes.get_mut(pairing_id) else {
            return Err(0);
        };
        if ob.accept_inbound(seq) {
            Ok(seq)
        } else {
            Err(ob.outbox().last_received())
        }
    }

    fn flush(&self, clean: bool) {
        let mut inner = self.lock();
        for ob in inner.outboxes.values_mut() {
            let _ = if clean { ob.flush_clean() } else { ob.flush() };
        }
    }

    fn set_advert(&self, endpoint: Option<DirectEndpoint>) {
        if let Ok(mut slot) = self.advert.write() {
            if *slot != endpoint {
                crate::remote::debuglog::log(&format!("direct ADVERTISE {endpoint:?}"));
                *slot = endpoint;
            }
        }
    }
}

/// The [`OutboundRouter`] over [`Shared`].
struct Router(Arc<Shared>);

impl OutboundRouter for Router {
    fn route(&self, msg: RemoteOutbound) -> Option<RemoteOutbound> {
        match msg {
            RemoteOutbound::SendEnvelope {
                pairing_id,
                seq,
                sent_at_ms,
                nonce,
                ciphertext,
            } if self.0.is_direct(pairing_id.as_str()) => {
                self.0.push(EncryptedEnvelope {
                    pairing_id,
                    seq,
                    sender: Role::Desktop,
                    sent_at_ms,
                    nonce,
                    ciphertext,
                });
                None
            }
            // The direct session acks the phone's envelopes itself; an explicit
            // app ack for a direct pairing has nowhere relay-side to go.
            RemoteOutbound::Ack { pairing_id, .. } if self.0.is_direct(pairing_id.as_str()) => None,
            // Act now (the relay may be unreachable) and still let the relay
            // client drop it from its in-memory state when it next runs.
            RemoteOutbound::Unpair { pairing_id } if self.0.is_direct(pairing_id.as_str()) => {
                self.0.forget(pairing_id.as_str());
                Some(RemoteOutbound::Unpair { pairing_id })
            }
            other => Some(other),
        }
    }
}

/// Wraps the relay client's [`RemoteStore`] so the direct link follows every
/// pairing change the client persists (see the module docs).
pub struct DirectAwareStore {
    shared: Arc<Shared>,
    inner: Arc<dyn RemoteStore>,
}

impl RemoteStore for DirectAwareStore {
    fn load(&self) -> RemoteState {
        let mut state = self.inner.load();
        self.shared.filter_tombstones(&mut state);
        state
    }

    fn save(&self, state: &RemoteState) {
        let mut persist = self
            .shared
            .persist
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut state = state.clone();
        self.shared.filter_tombstones(&mut state);
        let added = self.shared.sync_from_state(&state);
        self.inner.save(&state);
        persist.last = Some(state);
        drop(persist);
        // After the save, so the record is durable before anyone acts on it;
        // the client sends its own `PairingClaimed` right after this returns.
        for pid in added {
            self.shared.inbound.send(RemoteInbound::DirectTransport {
                pairing_id: PairingId::new(pid),
            });
        }
    }
}

/// A running direct link. Dropping it stops the listeners and sessions and
/// writes every outbox with its real high-water mark (an orderly shutdown).
pub struct DirectLink {
    shared: Arc<Shared>,
    stop: watch::Sender<bool>,
}

impl DirectLink {
    /// Start the direct link: open the outboxes of `initial`'s direct pairings,
    /// bind per `options.bind`, and announce each registered pairing to the app
    /// (so the bridge queues into its outbox even before the phone connects).
    /// `None` only when the shared runtime is unavailable.
    pub fn start(
        options: DirectOptions,
        initial: &RemoteState,
        inbound_tx: Sender<RemoteInbound>,
    ) -> Option<DirectLink> {
        let runtime = crate::remote::runtime::try_shared()?;
        let (outbox_tick, _) = watch::channel(0u64);
        let (registry_tick, _) = watch::channel(0u64);
        let shared = Arc::new(Shared {
            fs: options.fs,
            state_dir: options.state_dir,
            limits: options.limits,
            inbound: InboundTx(Mutex::new(inbound_tx)),
            inner: Mutex::new(Inner::default()),
            persist: Mutex::new(Persist::default()),
            outbox_tick,
            registry_tick,
            advert: Arc::new(RwLock::new(None)),
            machine_name: options.machine_name,
            bound: Mutex::new(Vec::new()),
        });
        for pid in shared.sync_from_state(initial) {
            let pairing_id = PairingId::new(pid.clone());
            shared.inbound.send(RemoteInbound::DirectTransport {
                pairing_id: pairing_id.clone(),
            });
            let peer_device_id = initial
                .pairing(&pid)
                .and_then(|p| p.peer_device_id.clone())
                .map(DeviceId::new);
            shared.inbound.send(RemoteInbound::Paired {
                pairing_id,
                peer_device_id,
            });
        }
        let (stop, stop_rx) = watch::channel(false);
        server::start(runtime, shared.clone(), options.bind, stop_rx);
        Some(DirectLink { shared, stop })
    }

    /// The router to install on the relay client's outbound channel.
    pub fn router(&self) -> Arc<dyn OutboundRouter> {
        Arc::new(Router(self.shared.clone()))
    }

    /// The endpoint slot the relay client reads when building an offer.
    pub fn advert(&self) -> DirectAdvert {
        self.shared.advert.clone()
    }

    /// The endpoint currently advertised (`None` while nothing is bound).
    pub fn endpoint(&self) -> Option<DirectEndpoint> {
        advertised(&self.shared.advert)
    }

    /// Wrap the relay client's store (see [`DirectAwareStore`]).
    pub fn wrap_store(&self, inner: Box<dyn RemoteStore>) -> Box<dyn RemoteStore> {
        let inner: Arc<dyn RemoteStore> = Arc::from(inner);
        self.shared
            .persist
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .store = Some(inner.clone());
        Box::new(DirectAwareStore {
            shared: self.shared.clone(),
            inner,
        })
    }

    /// The highest outbound `seq` the outbox has accepted for `pairing_id`
    /// (floored past anything a crashed process could have sent) — the startup
    /// floor for the bridge's counter, since a direct pairing's envelopes never
    /// pass through the relay client that persists `last_sent_seq`.
    pub fn outbound_high_water(&self, pairing_id: &str) -> Option<u64> {
        let inner = self.shared.lock();
        inner
            .outboxes
            .get(pairing_id)
            .map(|o| o.outbox().high_water())
    }

    /// The listener addresses currently bound.
    pub fn local_addrs(&self) -> Vec<SocketAddr> {
        self.shared
            .bound
            .lock()
            .map(|b| b.clone())
            .unwrap_or_default()
    }

    /// Stop now (equivalent to dropping).
    pub fn stop(self) {}
}

impl Drop for DirectLink {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.shared.flush(true);
    }
}

#[cfg(test)]
mod tests;
