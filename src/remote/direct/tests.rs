//! Direct-link unit tests: routing per pairing, the store wrapper's registry
//! sync and tombstones, and authentication. The wire protocol end to end is in
//! `tests/remote_direct.rs`.

use super::*;
use crate::remote::identity::DeviceIdentity;
use crate::remote::state::{Pairing, PairingTransport};
use crate::testing::FakeFs;
use std::sync::mpsc::{channel, Receiver};

struct MemStore(Mutex<RemoteState>);

impl RemoteStore for MemStore {
    fn load(&self) -> RemoteState {
        self.0.lock().unwrap().clone()
    }
    fn save(&self, state: &RemoteState) {
        *self.0.lock().unwrap() = state.clone();
    }
}

/// A store whose contents the test can inspect after handing it over.
struct SharedStore(Arc<MemStore>);

impl RemoteStore for SharedStore {
    fn load(&self) -> RemoteState {
        self.0.load()
    }
    fn save(&self, state: &RemoteState) {
        self.0.save(state)
    }
}

const ROOT: &str = "/home/u/.flightdeck/remote-direct";

fn direct_pairing(id: &str, phone: &DeviceIdentity) -> Pairing {
    let mut p = Pairing::new(id);
    p.peer_device_id = Some(phone.device_id().to_string());
    p.peer_key_agreement_public_key = Some(phone.public_key_base64());
    p.peer_device_public_key = Some(phone.public_key_base64());
    p.claim_token = Some("4729".to_string());
    p.established = true;
    p.transport = PairingTransport::Direct;
    p
}

fn start(state: &RemoteState) -> (DirectLink, Receiver<RemoteInbound>, Arc<FakeFs>) {
    let fs = Arc::new(FakeFs::new());
    let (tx, rx) = channel();
    let link = DirectLink::start(
        DirectOptions {
            state_dir: PathBuf::from(ROOT),
            fs: fs.clone(),
            bind: BindPlan::None,
            limits: OutboxLimits::default(),
            machine_name: Some("Test Mac".to_string()),
        },
        state,
        tx,
    )
    .expect("a test host has a runtime");
    (link, rx, fs)
}

fn drain(rx: &Receiver<RemoteInbound>) -> Vec<RemoteInbound> {
    rx.try_iter().collect()
}

fn send_envelope(pairing: &str, seq: u64) -> RemoteOutbound {
    RemoteOutbound::SendEnvelope {
        pairing_id: PairingId::new(pairing),
        seq,
        sent_at_ms: 1_000,
        nonce: "bm9uY2U=".to_string(),
        ciphertext: "Y2lwaGVy".to_string(),
    }
}

#[test]
fn needed_when_enabled_or_a_direct_pairing_exists() {
    let mut cfg = RemoteConfig::default();
    let mut state = RemoteState::default();
    state.pairings.push(Pairing::new("relay_one"));
    assert!(!needed(&cfg, &state));
    cfg.direct_enabled = true;
    assert!(needed(&cfg, &state));
    cfg.direct_enabled = false;
    let mut p = Pairing::new("direct_one");
    p.transport = PairingTransport::Direct;
    state.pairings.push(p);
    assert!(
        needed(&cfg, &state),
        "a direct pairing needs its outbox even with the listener switched off"
    );
}

#[test]
fn for_config_listens_only_when_enabled_and_keeps_outboxes_beside_remote_json() {
    let path = Path::new("/home/u/.flightdeck/remote.json");
    let mut cfg = RemoteConfig::default();
    let opts = DirectOptions::for_config(&cfg, path);
    assert!(matches!(opts.bind, BindPlan::None));
    assert_eq!(opts.state_dir, PathBuf::from(ROOT));
    cfg.direct_enabled = true;
    cfg.direct_port = 47811;
    match DirectOptions::for_config(&cfg, path).bind {
        BindPlan::Tailscale { port, .. } => assert_eq!(port, 47811),
        _ => panic!("enabled must listen on Tailscale"),
    }
}

#[test]
fn startup_announces_each_direct_pairing_and_relay_ones_stay_relay() {
    let phone = DeviceIdentity::generate();
    let mut state = RemoteState::default();
    state.pairings.push(direct_pairing("pair_d", &phone));
    state.pairings.push(Pairing::new("pair_r"));
    let (_link, rx, _fs) = start(&state);
    let events = drain(&rx);
    assert!(events.iter().any(
        |e| matches!(e, RemoteInbound::DirectTransport { pairing_id } if pairing_id.as_str() == "pair_d")
    ));
    assert!(events.iter().any(|e| matches!(
        e,
        RemoteInbound::Paired { pairing_id, peer_device_id: Some(d) }
            if pairing_id.as_str() == "pair_d" && d.as_str() == phone.device_id()
    )));
    assert!(
        !events.iter().any(|e| matches!(
            e,
            RemoteInbound::DirectTransport { pairing_id } | RemoteInbound::Paired { pairing_id, .. }
                if pairing_id.as_str() == "pair_r"
        )),
        "a relay-mode pairing is not the direct link's"
    );
}

#[test]
fn the_router_keeps_direct_envelopes_off_the_relay_and_passes_the_rest() {
    let phone = DeviceIdentity::generate();
    let mut state = RemoteState::default();
    state.pairings.push(direct_pairing("pair_d", &phone));
    state.pairings.push(Pairing::new("pair_r"));
    let (link, _rx, _fs) = start(&state);
    let router = link.router();

    assert!(router.route(send_envelope("pair_d", 1)).is_none(), "taken");
    assert!(router.route(send_envelope("pair_d", 2)).is_none(), "taken");
    assert_eq!(link.outbound_high_water("pair_d"), Some(2));
    assert!(matches!(
        router.route(send_envelope("pair_r", 7)),
        Some(RemoteOutbound::SendEnvelope { seq: 7, .. })
    ));
    assert_eq!(link.outbound_high_water("pair_r"), None);
    assert!(router
        .route(RemoteOutbound::Ack {
            pairing_id: PairingId::new("pair_d"),
            cursor: 3
        })
        .is_none());
    assert!(router
        .route(RemoteOutbound::RequestPairing {
            claim_token_hint: None
        })
        .is_some());
}

#[test]
fn an_offered_not_yet_claimed_direct_pairing_never_reaches_the_relay() {
    let mut state = RemoteState::default();
    let mut p = Pairing::new("pair_offered");
    p.transport = PairingTransport::Direct;
    state.pairings.push(p);
    let (link, _rx, _fs) = start(&state);
    assert!(link
        .router()
        .route(send_envelope("pair_offered", 1))
        .is_none());
    assert_eq!(
        link.outbound_high_water("pair_offered"),
        None,
        "no outbox before the claim"
    );
}

#[test]
fn a_claim_saved_by_the_client_registers_the_pairing() {
    let (link, rx, _fs) = start(&RemoteState::default());
    let inner = Arc::new(MemStore(Mutex::new(RemoteState::default())));
    let store = link.wrap_store(Box::new(SharedStore(inner.clone())));
    let phone = DeviceIdentity::generate();

    let mut state = RemoteState::default();
    let mut offered = Pairing::new("pair_new");
    offered.transport = PairingTransport::Direct;
    state.pairings.push(offered);
    store.save(&state);
    assert!(
        drain(&rx).is_empty(),
        "an unclaimed pairing is not authenticable yet"
    );

    state.pairings[0] = direct_pairing("pair_new", &phone);
    store.save(&state);
    assert!(drain(&rx).iter().any(
        |e| matches!(e, RemoteInbound::DirectTransport { pairing_id } if pairing_id.as_str() == "pair_new")
    ));
    assert_eq!(link.outbound_high_water("pair_new"), Some(0));
    assert_eq!(
        inner.load().pairings.len(),
        1,
        "the save reached the real store"
    );

    // A relay revoke / unpair / retirement is the client saving without it.
    link.router().route(send_envelope("pair_new", 1));
    state.pairings.clear();
    store.save(&state);
    assert_eq!(link.outbound_high_water("pair_new"), None);
}

#[test]
fn removing_a_pairing_deletes_its_outbox_on_disk() {
    let phone = DeviceIdentity::generate();
    let mut state = RemoteState::default();
    state.pairings.push(direct_pairing("pair_d", &phone));
    let (link, _rx, fs) = start(&state);
    let store = link.wrap_store(Box::new(MemStore(Mutex::new(state.clone()))));
    link.router().route(send_envelope("pair_d", 1));
    let dir = outbox::pairing_dir(Path::new(ROOT), "pair_d");
    assert!(fs.exists(&dir), "the first push is written through");
    store.save(&RemoteState::default());
    assert!(!fs.exists(&dir));
}

#[test]
fn forgetting_tombstones_the_pairing_out_of_every_save_and_load() {
    let phone = DeviceIdentity::generate();
    let mut state = RemoteState::default();
    state.pairings.push(direct_pairing("pair_d", &phone));
    state.pairings.push(Pairing::new("pair_r"));
    let (link, _rx, _fs) = start(&state);
    let inner = Arc::new(MemStore(Mutex::new(state.clone())));
    let store = link.wrap_store(Box::new(SharedStore(inner.clone())));

    // The unpair path: the router acts at once and still forwards it.
    let fwd = link.router().route(RemoteOutbound::Unpair {
        pairing_id: PairingId::new("pair_d"),
    });
    assert!(matches!(fwd, Some(RemoteOutbound::Unpair { .. })));
    assert_eq!(
        inner.load().pairing_ids(),
        vec!["pair_r".to_string()],
        "remote.json is rewritten without it immediately"
    );
    // The client's in-memory copy still has it; its next save must not
    // resurrect it, and a load must not hand it back.
    store.save(&state);
    assert_eq!(inner.load().pairing_ids(), vec!["pair_r".to_string()]);
    assert_eq!(store.load().pairing_ids(), vec!["pair_r".to_string()]);
    // Its envelopes are still swallowed, never sent to the relay.
    assert!(link.router().route(send_envelope("pair_d", 5)).is_none());
    assert_eq!(link.outbound_high_water("pair_d"), None);
}

fn sign(id: &DeviceIdentity, nonce: &[u8]) -> String {
    STANDARD.encode(id.sign(&direct_auth_message(nonce)))
}

#[test]
fn authentication_accepts_only_the_paired_phone_signing_the_direct_message() {
    let phone = DeviceIdentity::generate();
    let other = DeviceIdentity::generate();
    let mut state = RemoteState::default();
    state.pairings.push(direct_pairing("pair_d", &phone));
    let mut relay_mode = direct_pairing("pair_r", &phone);
    relay_mode.transport = PairingTransport::Relay;
    state.pairings.push(relay_mode);
    let (link, _rx, _fs) = start(&state);
    let shared = &link.shared;
    let nonce = [7u8; 32];
    let pid = |s: &str| vec![PairingId::new(s)];

    assert_eq!(
        shared.authenticate(
            phone.device_id(),
            &pid("pair_d"),
            &nonce,
            &sign(&phone, &nonce)
        ),
        Ok(())
    );
    // The relay's message (the bare nonce) is not valid here: domain separation.
    let bare = STANDARD.encode(phone.sign(&nonce));
    assert!(shared
        .authenticate(phone.device_id(), &pid("pair_d"), &nonce, &bare)
        .is_err());
    // A different key, even claiming the right device id.
    assert!(shared
        .authenticate(
            phone.device_id(),
            &pid("pair_d"),
            &nonce,
            &sign(&other, &nonce)
        )
        .is_err());
    // The right key over a different nonce (a replayed response).
    assert!(shared
        .authenticate(
            phone.device_id(),
            &pid("pair_d"),
            &nonce,
            &sign(&phone, &[8u8; 32])
        )
        .is_err());
    // Another device id.
    assert!(shared
        .authenticate(
            other.device_id(),
            &pid("pair_d"),
            &nonce,
            &sign(&other, &nonce)
        )
        .is_err());
    // Unknown pairing, a relay-mode pairing, nothing requested, garbage.
    assert!(shared
        .authenticate(
            phone.device_id(),
            &pid("pair_x"),
            &nonce,
            &sign(&phone, &nonce)
        )
        .is_err());
    assert!(shared
        .authenticate(
            phone.device_id(),
            &pid("pair_r"),
            &nonce,
            &sign(&phone, &nonce)
        )
        .is_err());
    assert!(shared
        .authenticate(phone.device_id(), &[], &nonce, &sign(&phone, &nonce))
        .is_err());
    assert!(shared
        .authenticate(phone.device_id(), &pid("pair_d"), &nonce, "not base64!")
        .is_err());
    // Every requested pairing must pass: one bad id fails the lot.
    let both = vec![PairingId::new("pair_d"), PairingId::new("pair_x")];
    assert!(shared
        .authenticate(phone.device_id(), &both, &nonce, &sign(&phone, &nonce))
        .is_err());

    // Revoked: refused from then on.
    shared.forget("pair_d");
    assert!(shared
        .authenticate(
            phone.device_id(),
            &pid("pair_d"),
            &nonce,
            &sign(&phone, &nonce)
        )
        .is_err());
}

#[test]
fn inbound_dedup_and_ack_go_through_the_pairing_outbox() {
    let phone = DeviceIdentity::generate();
    let mut state = RemoteState::default();
    state.pairings.push(direct_pairing("pair_d", &phone));
    let (link, _rx, _fs) = start(&state);
    let shared = &link.shared;
    assert_eq!(shared.accept_inbound("pair_d", 1), Ok(1));
    assert_eq!(shared.accept_inbound("pair_d", 2), Ok(2));
    assert_eq!(shared.accept_inbound("pair_d", 2), Err(2));
    assert_eq!(shared.accept_inbound("pair_x", 1), Err(0));
    let router = link.router();
    for s in 1..=3 {
        router.route(send_envelope("pair_d", s));
    }
    shared.ack("pair_d", 2);
    assert_eq!(
        shared
            .after("pair_d", 0)
            .iter()
            .map(|e| e.seq)
            .collect::<Vec<_>>(),
        vec![3]
    );
}

#[test]
fn dropping_the_link_writes_outboxes_for_an_orderly_restart() {
    let phone = DeviceIdentity::generate();
    let mut state = RemoteState::default();
    state.pairings.push(direct_pairing("pair_d", &phone));
    let (link, _rx, fs) = start(&state);
    for s in 1..=3 {
        link.router().route(send_envelope("pair_d", s));
    }
    drop(link);
    let reopened = OutboxFile::open(
        fs.clone(),
        Path::new(ROOT),
        "pair_d",
        OutboxLimits::default(),
    );
    assert_eq!(
        reopened.outbox().high_water(),
        3,
        "no seq jump after a clean stop"
    );
    assert_eq!(reopened.outbox().len(), 3);
}
