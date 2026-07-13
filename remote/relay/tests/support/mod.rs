//! Shared test support: an in-process relay plus an authenticating WebSocket
//! client that speaks the real §5 handshake (ECDSA P-256 keys, challenge
//! signing, pairing bootstrap). Future relay tasks reuse this by `mod support;`
//! in their own `tests/*.rs` files.
//!
//! Not every helper is exercised by every test binary, so the module is
//! `dead_code`-tolerant.
#![allow(dead_code)]

use std::time::Duration;

use flightdeck_relay::{
    app,
    config::{Config, LogFormat},
};
use flightdeck_remote_protocol::{
    ClientInfo, DeviceId, PairingId, RelayFrame, Role, PROTOCOL_VERSION,
};
use futures_util::{SinkExt, StreamExt};
use p256::ecdsa::{signature::Signer, Signature, SigningKey, VerifyingKey};
use rand_core::OsRng;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async, tungstenite::Message as WsMessage, MaybeTlsStream, WebSocketStream,
};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Base64 (standard, padded) — the wire convention for all binary fields.
pub fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A correctly-sized (64-byte) but bogus signature, for auth-failure tests.
pub fn bogus_signature() -> String {
    b64(&[0u8; 64])
}

fn public_key_b64(vk: &VerifyingKey) -> String {
    b64(vk.to_encoded_point(false).as_bytes())
}

/// Spawn the relay on an ephemeral port with the given tuning, returning the
/// base `http://…` URL. `queue_max` sets `QUEUE_MAX_PER_PAIRING`.
pub async fn spawn_app_with(queue_max: usize, auth_timeout_secs: u64) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");

    let mut config = Config::new(addr.port(), LogFormat::Pretty, "test-sha");
    config.queue_max_per_pairing = queue_max;
    config.auth_timeout_secs = auth_timeout_secs;

    tokio::spawn(async move {
        axum::serve(listener, app(config)).await.expect("serve");
    });
    format!("http://{addr}")
}

/// Spawn with defaults suitable for most tests.
pub async fn spawn_app() -> String {
    spawn_app_with(1000, 10).await
}

/// A connected WebSocket client with a P-256 identity, mid-handshake or beyond.
pub struct TestClient {
    ws: Ws,
    signing_key: SigningKey,
    pub public_key_b64: String,
    /// A **separate** software P-256 key-agreement public key (base64 SEC1),
    /// mirroring the iOS split where the identity key is a Secure-Enclave
    /// signing key and the KA key is a distinct software key.
    pub key_agreement_public_key_b64: String,
    pub device_id: DeviceId,
    pub role: Role,
    /// The most recent `auth_challenge` nonce (decoded), ready to sign.
    nonce: Vec<u8>,
}

impl TestClient {
    /// Open a connection with a fresh identity; see [`Self::connect_with_key`].
    pub async fn connect(base_url: &str, role: Role, device_id: &str) -> Self {
        let key = SigningKey::random(&mut OsRng);
        Self::connect_with_key(base_url, role, device_id, key).await
    }

    /// The connection's private identity key, cloned — pass it to
    /// [`Self::connect_with_key`] to reconnect as the same device (the relay
    /// verifies against the key registered on the first connection).
    pub fn key(&self) -> SigningKey {
        self.signing_key.clone()
    }

    /// Cleanly close the connection (sends a WebSocket Close), triggering the
    /// relay's disconnect-presence path for any peer.
    pub async fn close(mut self) {
        let _ = self.ws.close(None).await;
    }

    /// Open a connection with a specific identity key, send `hello`, and consume
    /// `hello_ok` + `auth_challenge`. Leaves the client in the pre-auth phase.
    pub async fn connect_with_key(
        base_url: &str,
        role: Role,
        device_id: &str,
        signing_key: SigningKey,
    ) -> Self {
        let ws_url = format!("{}/ws", base_url.replacen("http://", "ws://", 1));
        let (ws, resp) = connect_async(ws_url).await.expect("ws handshake");
        assert_eq!(resp.status(), 101, "expected protocol switch");

        // A distinct software P-256 key for ECDH key agreement. Its private
        // scalar is irrelevant to the relay tests (the relay only stores/relays
        // the public point); we just need a well-formed SEC1 point. Computed
        // before `public_key_b64` shadows the helper fn of the same name.
        let key_agreement_public_key_b64 =
            public_key_b64(SigningKey::random(&mut OsRng).verifying_key());
        let public_key_b64 = public_key_b64(signing_key.verifying_key());

        let mut client = Self {
            ws,
            signing_key,
            public_key_b64,
            key_agreement_public_key_b64,
            device_id: DeviceId::new(device_id),
            role,
            nonce: Vec::new(),
        };

        client
            .send(RelayFrame::Hello {
                protocol_version: PROTOCOL_VERSION,
                role,
                device_id: DeviceId::new(device_id),
                client: ClientInfo {
                    app_version: "test".into(),
                    platform: "test".into(),
                    os_version: None,
                },
            })
            .await;

        match client.recv().await {
            RelayFrame::HelloOk {
                protocol_version, ..
            } => {
                assert_eq!(protocol_version, PROTOCOL_VERSION)
            }
            other => panic!("expected hello_ok, got {other:?}"),
        }
        match client.recv().await {
            RelayFrame::AuthChallenge { nonce, .. } => {
                use base64::Engine as _;
                client.nonce = base64::engine::general_purpose::STANDARD
                    .decode(nonce)
                    .expect("nonce base64");
            }
            other => panic!("expected auth_challenge, got {other:?}"),
        }
        client
    }

    /// Send a raw frame.
    pub async fn send(&mut self, frame: RelayFrame) {
        let text = serde_json::to_string(&frame).expect("serialize frame");
        self.ws
            .send(WsMessage::Text(text.into()))
            .await
            .expect("ws send");
    }

    /// Receive the next relay frame (panics on close / non-text / timeout).
    pub async fn recv(&mut self) -> RelayFrame {
        let msg = tokio::time::timeout(Duration::from_secs(5), self.ws.next())
            .await
            .expect("recv timed out")
            .expect("stream ended")
            .expect("ws error");
        match msg {
            WsMessage::Text(text) => serde_json::from_str(&text).expect("parse frame"),
            other => panic!("expected text frame, got {other:?}"),
        }
    }

    /// Receive frames until one satisfies `pred`, returning it. Frames that do
    /// not match (e.g. interleaved presence) are discarded.
    pub async fn recv_until(&mut self, pred: impl Fn(&RelayFrame) -> bool) -> RelayFrame {
        loop {
            let frame = self.recv().await;
            if pred(&frame) {
                return frame;
            }
        }
    }

    /// Assert that no frame arrives within `ms` (e.g. resume replays nothing).
    pub async fn expect_idle(&mut self, ms: u64) {
        match tokio::time::timeout(Duration::from_millis(ms), self.ws.next()).await {
            Err(_) => {} // timed out → idle, as expected
            Ok(other) => panic!("expected no frame, got {other:?}"),
        }
    }

    fn sign_nonce(&self) -> String {
        let sig: Signature = self.signing_key.sign(&self.nonce);
        b64(&sig.to_bytes())
    }

    /// Desktop bootstrap: send `pairing_offer`, return `(pairing_id, claim_token)`.
    pub async fn offer_pairing(&mut self) -> (PairingId, String) {
        self.send(RelayFrame::PairingOffer {
            device_id: self.device_id.clone(),
            device_public_key: self.public_key_b64.clone(),
            key_agreement_public_key: self.key_agreement_public_key_b64.clone(),
            role: self.role,
        })
        .await;
        match self.recv().await {
            RelayFrame::PairingOfferOk {
                pairing_id,
                claim_token,
                ..
            } => (pairing_id, claim_token),
            other => panic!("expected pairing_offer_ok, got {other:?}"),
        }
    }

    /// Phone bootstrap: redeem `token`, return the joined `pairing_id`.
    pub async fn claim_pairing(&mut self, token: &str) -> PairingId {
        self.claim_pairing_full(token).await.0
    }

    /// Phone bootstrap: redeem `token`, returning both the joined `pairing_id`
    /// and the peer (desktop) key-agreement public key the relay hands back.
    pub async fn claim_pairing_full(&mut self, token: &str) -> (PairingId, Option<String>) {
        self.send(RelayFrame::PairingClaim {
            claim_token: token.to_string(),
            device_id: self.device_id.clone(),
            device_public_key: self.public_key_b64.clone(),
            key_agreement_public_key: self.key_agreement_public_key_b64.clone(),
            role: self.role,
        })
        .await;
        match self.recv().await {
            RelayFrame::PairingClaimed {
                pairing_id,
                peer_key_agreement_public_key,
                ..
            } => (pairing_id, peer_key_agreement_public_key),
            other => panic!("expected pairing_claimed, got {other:?}"),
        }
    }

    /// Sign the challenge and authenticate, activating `pairing_ids`. Returns
    /// the pairings the relay reports active in `auth_ok`.
    pub async fn authenticate(&mut self, pairing_ids: Vec<PairingId>) -> Vec<PairingId> {
        let signature = self.sign_nonce();
        self.send(RelayFrame::AuthResponse {
            device_id: self.device_id.clone(),
            signature,
            pairing_ids,
        })
        .await;
        match self.recv().await {
            RelayFrame::AuthOk { pairing_ids } => pairing_ids,
            other => panic!("expected auth_ok, got {other:?}"),
        }
    }
}
