//! Desktop pairing surface (Settings → Remote): the state machine that drives
//! the QR + 4-digit code overlay and finishes pairing, plus the artifacts it
//! needs (the `fdr1:` QR payload, the QR half-block art, and the E2E channel
//! derivation once a phone joins).
//!
//! # Flow (spec §5.2)
//!
//! ```text
//!   Idle ──begin──▶ Offering ──pairing_offer_ok──▶ Displaying{code, qr, expires}
//!                                                        │  pairing_claimed
//!                                                        ▼
//!                                                   Established
//! ```
//!
//! The desktop initiates: it asks the relay client to send a `pairing_offer`
//! carrying a **4-digit `claim_token_hint`** (which the relay issues verbatim
//! when free, so the human-typeable code is exactly those digits). The relay
//! replies `pairing_offer_ok { claim_token, expires_at_ms }`; the overlay then
//! shows the code and a QR encoding the same token, a fresh random
//! `pairing_secret`, and the relay URL. When the phone redeems the token the
//! relay sends `pairing_claimed { peer_key_agreement_public_key }` back on the
//! desktop's own connection — the moment the desktop derives the E2E channel.
//!
//! # Salt contract (reconciled — spec §7.1)
//!
//! The E2E HKDF **salt is always the `claim_token` UTF-8 bytes**, on *both* the
//! QR and the manual-code paths. The desktop derives the channel from the
//! `pairing_claimed` notification and therefore **cannot know which path the
//! phone used**, so a path-dependent salt (the 32-byte QR `pairing_secret` vs.
//! the token) would be underivable here. The `claim_token` is the one value both
//! endpoints always share regardless of path — the desktop mints it, the QR
//! carries it, and the 4-digit code *is* it — so it is the only deterministic
//! choice. A 4-digit token is low-entropy, but the salt is only defence in depth:
//! the channel's confidentiality rests on the static-static P-256 ECDH between
//! the key-agreement keys (whose private scalars never leave the devices), not on
//! the salt. Short TTL + single use + a per-connection relay rate limit bound the
//! token's exposure. The `pairing_secret` stays in the QR payload for wire
//! compatibility with the iOS decoder but is **not** used in key derivation.

use std::sync::Arc;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use rand_core::{OsRng, RngCore};
use serde::Serialize;

use crate::contracts::{FlightDeckError, Result};
use crate::remote::bridge::{OpenFn, SealFn};
use crate::remote::crypto::E2eChannel;

use flightdeck_remote_protocol::{PairingId, Role};

/// The QR payload scheme marker: "FlightDeck Remote, payload version 1" (matches
/// `ios/.../PairingModels.swift`).
const QR_SCHEME_PREFIX: &str = "fdr1:";

/// Number of random bytes in the QR `pairing_secret` (spec §5.2 — 32 bytes).
const PAIRING_SECRET_LEN: usize = 32;

/// The observable phase of a pairing attempt, for the overlay to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingPhase {
    /// Nothing in progress.
    Idle,
    /// A `pairing_offer` was sent; awaiting the relay's `pairing_offer_ok`.
    Offering,
    /// The code + QR are on screen; awaiting the phone's `pairing_claimed`.
    Displaying {
        /// The human-typeable claim token (the 4-digit code when the hint was
        /// honored).
        code: String,
        /// The full `fdr1:` QR payload string to render as a QR code.
        qr_payload: String,
        /// Relay wall-clock time (unix ms) after which the token expires.
        expires_at_ms: i64,
    },
    /// The phone joined and the E2E channel was derived — pairing complete.
    Established {
        /// The established pairing.
        pairing_id: PairingId,
    },
    /// Pairing could not complete (token expired, relay error, missing KA key).
    Failed {
        /// A short, honest message for the overlay.
        message: String,
    },
}

/// A desktop pairing attempt. Pure and I/O-free: it is fed relay frames
/// (as [`crate::remote::RemoteInbound`] the loop translates) and exposes the
/// current [`PairingPhase`] plus, once a phone joins, the material to derive the
/// E2E channel.
#[derive(Debug)]
pub struct PairingSession {
    phase: PairingPhase,
    /// The relay URL to embed in the QR (so manual-code entry is not required
    /// to know it out of band).
    relay_url: String,
    /// The 4-digit code we asked the relay to use as the claim token.
    hint: String,
    /// The QR `pairing_secret` (base64url, no padding) — carried in the payload
    /// for wire compatibility; not part of key derivation (see module docs).
    pairing_secret_b64url: String,
    /// The pairing id, once offered.
    pairing_id: Option<PairingId>,
    /// The effective claim token (salt source), once offered.
    claim_token: Option<String>,
    /// The peer (phone) key-agreement public key, once claimed.
    peer_ka_b64: Option<String>,
}

impl PairingSession {
    /// Start a fresh attempt: generate a 4-digit code and a random
    /// `pairing_secret`, entering [`PairingPhase::Offering`]. The caller sends
    /// [`crate::remote::RemoteOutbound::RequestPairing`] with [`Self::hint`].
    pub fn begin(relay_url: impl Into<String>) -> Self {
        Self::begin_with_hint(relay_url, four_digit_code())
    }

    /// Start a fresh attempt with an explicit 4-digit code `hint` instead of a
    /// random one, entering [`PairingPhase::Offering`]. Because the hint flows
    /// through to the relay as the `claim_token_hint` (issued verbatim when
    /// free), a fixed hint yields a **deterministic** claim token.
    ///
    /// This is a test / E2E seam: the interactive desktop always uses
    /// [`Self::begin`] (random code); the fixed-hint path exists so an automated
    /// harness can pair non-interactively with a known code (see the
    /// `FLIGHTDECK_REMOTE_AUTOPAIR` startup seam in `lib.rs`).
    pub fn begin_with_hint(relay_url: impl Into<String>, hint: impl Into<String>) -> Self {
        let mut secret = [0u8; PAIRING_SECRET_LEN];
        OsRng.fill_bytes(&mut secret);
        PairingSession {
            phase: PairingPhase::Offering,
            relay_url: relay_url.into(),
            hint: hint.into(),
            pairing_secret_b64url: URL_SAFE_NO_PAD.encode(secret),
            pairing_id: None,
            claim_token: None,
            peer_ka_b64: None,
        }
    }

    /// The 4-digit code hint to request from the relay.
    pub fn hint(&self) -> &str {
        &self.hint
    }

    /// The current phase (for rendering).
    pub fn phase(&self) -> &PairingPhase {
        &self.phase
    }

    /// Whether this attempt reached [`PairingPhase::Established`].
    pub fn is_established(&self) -> bool {
        matches!(self.phase, PairingPhase::Established { .. })
    }

    /// The pairing id, once an offer has been minted (for unpair / bookkeeping).
    pub fn pairing_id(&self) -> Option<&PairingId> {
        self.pairing_id.as_ref()
    }

    /// Handle `pairing_offer_ok`: record the token and move to
    /// [`PairingPhase::Displaying`], building the QR payload. Ignored unless we
    /// are still offering.
    pub fn on_offered(&mut self, pairing_id: PairingId, claim_token: String, expires_at_ms: i64) {
        if !matches!(self.phase, PairingPhase::Offering) {
            return;
        }
        let qr_payload =
            build_qr_payload(&claim_token, &self.pairing_secret_b64url, &self.relay_url);
        self.pairing_id = Some(pairing_id);
        self.claim_token = Some(claim_token.clone());
        self.phase = PairingPhase::Displaying {
            code: claim_token,
            qr_payload,
            expires_at_ms,
        };
    }

    /// Handle `pairing_claimed`: record the peer key-agreement key and, if
    /// present, move to [`PairingPhase::Established`]. A missing KA key means the
    /// relay could not complete the exchange, which fails the attempt.
    /// Returns `true` when it just became established.
    pub fn on_claimed(
        &mut self,
        pairing_id: PairingId,
        peer_key_agreement_public_key: Option<String>,
    ) -> bool {
        // Only react while displaying the code for this pairing.
        if !matches!(self.phase, PairingPhase::Displaying { .. }) {
            return false;
        }
        if self.pairing_id.as_ref() != Some(&pairing_id) {
            return false;
        }
        match peer_key_agreement_public_key {
            Some(ka) => {
                self.peer_ka_b64 = Some(ka);
                self.phase = PairingPhase::Established { pairing_id };
                true
            }
            None => {
                self.phase = PairingPhase::Failed {
                    message: "The phone joined but the relay did not return its key. Try again."
                        .to_string(),
                };
                false
            }
        }
    }

    /// Mark the attempt failed with an honest message (relay error / timeout).
    pub fn fail(&mut self, message: impl Into<String>) {
        self.phase = PairingPhase::Failed {
            message: message.into(),
        };
    }

    /// The seconds remaining until the token expires, given the current wall
    /// clock, saturating at zero. `None` unless a code is on screen.
    pub fn seconds_remaining(&self, now_ms: i64) -> Option<i64> {
        match &self.phase {
            PairingPhase::Displaying { expires_at_ms, .. } => {
                Some(((*expires_at_ms - now_ms) / 1000).max(0))
            }
            _ => None,
        }
    }

    /// Derive the live E2E sealer/opener for the established pairing from this
    /// desktop's identity private scalar (reused as the key-agreement key). The
    /// salt is the claim-token bytes (see module docs). Returns the pairing id
    /// plus the `(seal, open)` pair the bridge installs, or an error if the
    /// attempt is not established or the peer key is malformed.
    pub fn derive_channel(
        &self,
        identity_private_scalar: &[u8],
    ) -> Result<(PairingId, SealFn, OpenFn)> {
        let PairingPhase::Established { pairing_id } = &self.phase else {
            return Err(FlightDeckError::State(
                "pairing not established; cannot derive channel".to_string(),
            ));
        };
        let peer_ka_b64 = self.peer_ka_b64.as_ref().ok_or_else(|| {
            FlightDeckError::State("pairing has no peer key-agreement key".to_string())
        })?;
        let claim_token = self.claim_token.as_ref().ok_or_else(|| {
            FlightDeckError::State("pairing has no claim token for the salt".to_string())
        })?;
        let (seal, open) = build_channel(
            identity_private_scalar,
            peer_ka_b64,
            pairing_id.as_str(),
            claim_token,
        )?;
        Ok((pairing_id.clone(), seal, open))
    }
}

/// Build the `(seal, open)` pair for a desktop endpoint from the raw inputs.
/// Shared by the runtime pairing flow and the startup go-live for an already
/// established pairing. `peer_ka_b64` is base64 (standard, padded) X9.63; the
/// salt is the `claim_token` UTF-8 bytes (spec §7.1, reconciled contract).
pub fn build_channel(
    identity_private_scalar: &[u8],
    peer_ka_b64: &str,
    pairing_id: &str,
    claim_token: &str,
) -> Result<(SealFn, OpenFn)> {
    let peer_ka = STANDARD
        .decode(peer_ka_b64)
        .map_err(|e| FlightDeckError::State(format!("peer KA key not base64: {e}")))?;
    let channel = E2eChannel::derive(
        identity_private_scalar,
        &peer_ka,
        pairing_id,
        claim_token.as_bytes(),
        Role::Desktop,
    )?;
    let channel = Arc::new(channel);
    let seal_channel = Arc::clone(&channel);
    let seal: SealFn =
        Box::new(move |plain, seq, sent_at_ms| seal_channel.seal(plain, seq, sent_at_ms).ok());
    let open: OpenFn = Box::new(move |seq, sender, sent_at_ms, nonce, ciphertext| {
        channel
            .open(seq, sender, sent_at_ms, nonce, ciphertext)
            .ok()
    });
    Ok((seal, open))
}

/// The JSON body of the QR payload (spec §5.2). Field order is fixed so the
/// encoded output is deterministic; the iOS side decodes order-independently.
#[derive(Serialize)]
struct QrPayloadJson<'a> {
    claim_token: &'a str,
    pairing_secret: &'a str,
    relay_url: &'a str,
}

/// Build the `"fdr1:" + base64url_nopad(JSON)` QR payload string, byte-for-byte
/// matching the format in `ios/.../PairingModels.swift`.
pub fn build_qr_payload(claim_token: &str, pairing_secret_b64url: &str, relay_url: &str) -> String {
    let json = serde_json::to_vec(&QrPayloadJson {
        claim_token,
        pairing_secret: pairing_secret_b64url,
        relay_url,
    })
    .expect("qr payload serializes");
    format!("{QR_SCHEME_PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
}

/// A fresh, uniformly-distributed 4-digit decimal code (`"0000".."9999"`), from
/// the OS CSPRNG. Rejection-samples to avoid modulo bias.
fn four_digit_code() -> String {
    loop {
        let mut buf = [0u8; 2];
        OsRng.fill_bytes(&mut buf);
        let n = u16::from_be_bytes(buf);
        // Largest multiple of 10000 that fits in u16 is 60000; reject above it.
        if n < 60_000 {
            return format!("{:04}", n % 10_000);
        }
    }
}

// ---------------------------------------------------------------------------
// QR half-block art (rendered by the TUI overlay)
// ---------------------------------------------------------------------------

/// A quiet zone of this many light modules is added around the QR so scanners
/// can lock on (spec / ISO recommends 4; kept minimal for terminal real estate).
const QR_QUIET_ZONE: usize = 2;

/// Rendered QR art: one string per text row, each row packing **two** vertical
/// QR modules into a half-block cell. Callers render it in black-on-white so a
/// phone camera can scan it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrArt {
    /// The half-block rows, top to bottom. Every row has the same char count.
    pub rows: Vec<String>,
    /// Width in terminal cells (also the char count of each row).
    pub width: usize,
}

/// Encode `data` as a QR code and render it as black-on-white half-block art,
/// or `None` if the data cannot be encoded (too large for any version).
pub fn qr_art(data: &str) -> Option<QrArt> {
    use qrcode::{EcLevel, QrCode};

    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M).ok()?;
    let modules = code.width();
    let colors = code.to_colors();

    // Build a padded boolean grid (true = dark module) including the quiet zone.
    let side = modules + 2 * QR_QUIET_ZONE;
    let dark = |r: usize, c: usize| -> bool {
        if r < QR_QUIET_ZONE
            || c < QR_QUIET_ZONE
            || r >= QR_QUIET_ZONE + modules
            || c >= QR_QUIET_ZONE + modules
        {
            return false;
        }
        let mr = r - QR_QUIET_ZONE;
        let mc = c - QR_QUIET_ZONE;
        colors[mr * modules + mc] == qrcode::Color::Dark
    };

    let mut rows = Vec::with_capacity(side.div_ceil(2));
    let mut row = 0;
    while row < side {
        let mut line = String::with_capacity(side);
        for col in 0..side {
            let top = dark(row, col);
            let bottom = row + 1 < side && dark(row + 1, col);
            line.push(half_block(top, bottom));
        }
        rows.push(line);
        row += 2;
    }
    Some(QrArt { rows, width: side })
}

/// The half-block glyph for a (top, bottom) module pair. Rendered black-on-white:
/// the upper half-block `▀` paints the top cell in the foreground colour and
/// leaves the bottom in the background colour, so with black-on-white a dark
/// module is black and a light one white.
fn half_block(top_dark: bool, bottom_dark: bool) -> char {
    match (top_dark, bottom_dark) {
        (true, true) => '█',
        (true, false) => '▀',
        (false, true) => '▄',
        (false, false) => ' ',
    }
}

#[cfg(test)]
mod tests;
