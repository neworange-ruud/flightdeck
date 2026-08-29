//! Access credentials for the browser control surface (`specs/WEB_INTERFACE.md`
//! D5, D10, Q4).
//!
//! Pure logic and persistence: no HTTP, no server, no runtime. The axum layer in
//! [`crate::web::server`] owns the transport and calls into a
//! [`CredentialStore`] for every decision it makes about who is allowed in.
//!
//! # Two credentials, not one (Q4)
//!
//! ```text
//!   desktop overlay                              browser
//!   ───────────────                              ───────
//!   mint_bootstrap_code()
//!     ┌─────────────────┐   4 digits, ~120s,
//!     │  8 4 1 9   1:53 │   single use, shown         GET /#8419
//!     └─────────────────┘   with a countdown      ──────────────▶
//!                                                exchange_code()
//!                                                      │
//!                           persistent token,          ▼
//!                           long-lived, one per   ◀──────────────
//!                           browser, revocable    HttpOnly cookie
//!                                                       │
//!                                                verify_token()
//!                                                on every request
//! ```
//!
//! D10 needs a bookmark to keep working across host restarts, and a permanent
//! code left on screen is a poor idea. Splitting the two reconciles that: the
//! **bootstrap code** is short, short-lived and human-typeable (it is what the
//! desktop shows and the QR encodes); the **persistent token** is 256 bits of
//! CSPRNG output that no human ever sees, held by exactly one browser, and
//! revocable from the desktop at any time.
//!
//! # Why the URL *fragment* carries the code
//!
//! The browser is handed the code as `http://host:7420/#8419`, not
//! `?code=8419`. A fragment is never sent to the server: it does not appear in
//! the request line, so it cannot land in an access log, a proxy log, a
//! `Referer` header, or a crash report. The SPA reads `location.hash`, POSTs the
//! code to the exchange endpoint over the body, and strips the fragment from
//! history. A query string would have been logged by every intermediary that
//! logs request lines, which for a credential is exactly wrong.
//!
//! The same reasoning shapes this module's types: [`BootstrapCode`] and
//! [`AccessToken`] have **hand-written [`Debug`] implementations that redact the
//! secret**, and neither implements [`std::fmt::Display`]. A `tracing::debug!`
//! or a `dbg!` on a struct that transitively holds one cannot print it. Getting
//! a secret into a log should require typing `.reveal()`, which is easy to spot
//! in review.
//!
//! # What is stored, and what is not
//!
//! `~/.flightdeck/web.json` holds, per browser, the **SHA-256 of the token** —
//! never the token itself. Verification only needs to recognise a token, not
//! reproduce one, so the hash is sufficient; and a leaked `web.json` (a stray
//! backup, a synced home directory, another process reading it) then hands the
//! attacker nothing usable. The trade-off, stated honestly: the host can never
//! re-display a token, so a browser that loses its cookie must be re-bootstrapped
//! with a fresh code rather than "shown the token again". That is the right way
//! round — the recovery path is 4 digits and two minutes.
//!
//! A bare SHA-256 with no salt and no KDF is deliberate and sufficient *here*:
//! the input is 32 uniformly random bytes, so there is no dictionary to run and
//! no rainbow table to build. This reasoning does **not** transfer to anything
//! user-chosen; do not copy the pattern for a password.
//!
//! The bootstrap code lives in memory only. It is worth less than a token, it
//! expires in two minutes, and persisting it would make it survive the restart
//! that ought to kill it.
//!
//! # What the rate limiter does and does not protect against
//!
//! A 4-digit code is 10 000 possibilities — small enough that the limiter is
//! load-bearing, not decoration. Three defences stack:
//!
//! 1. **Per-address**: after [`RATE_LIMIT_MAX_FAILURES`] failures from one
//!    address that address is refused for [`RATE_LIMIT_LOCKOUT_MS`]. Per-address
//!    rather than global on purpose — a phone fat-fingering the code on the
//!    guest wifi must not lock the user's own desktop browser out of its own
//!    machine.
//! 2. **Per-code**: a live code is *burned* after
//!    [`BOOTSTRAP_CODE_MAX_FAILURES`] wrong guesses in total, from any number of
//!    addresses. This is the half the per-address limiter cannot do, because an
//!    attacker with many source addresses is not slowed by a per-address budget.
//! 3. **Time and single use**: the code is valid for
//!    [`BOOTSTRAP_CODE_TTL_MS`] and dies the first time it is exchanged.
//!
//! It does **not** protect against:
//!
//! * **Anyone who can read the screen.** The code and the QR are legible to a
//!   shoulder, a camera, or a screen share. That is Q7's territory: the honest
//!   mitigation is the manual reveal toggle, not a claimed detection we cannot
//!   deliver on every platform.
//! * **Anyone already on the socket's network** when the server is bound to
//!   `0.0.0.0` instead of the D5 loopback default. The limiter raises the cost
//!   of guessing; it does not make an exposed bind safe.
//! * **A spoofed or NAT-shared source address.** Addresses are attacker-supplied
//!   labels. Many attackers behind one NAT share a budget (they lock each other
//!   out — fine); one attacker rotating addresses gets a fresh budget each time
//!   (not fine — which is what defence 2 exists for). A flood of distinct
//!   addresses also evicts honest entries once
//!   [`RATE_LIMIT_MAX_TRACKED_ADDRESSES`] is reached, so the limiter degrades
//!   towards permissive rather than locking the machine out of itself.
//! * **A stolen token.** A token is a bearer credential; whoever holds it is in,
//!   until it is revoked. There is no proof-of-possession and no binding to the
//!   address it was issued to.
//! * **Timing side channels outside this module.** Comparisons here are constant
//!   time; the response *shapes* the server picks are its own problem.
//!
//! # Seams
//!
//! Everything touching the filesystem goes through [`FileSystem`] and everything
//! touching time through [`Clock`], per
//! `.agents/skills/flightdeck-architecture-seams`. That is what lets the tests
//! expire a 120-second code and serve a 60-second lockout without sleeping, and
//! without writing a byte into the developer's real `~/.flightdeck`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::contracts::{Clock, FileSystem, FlightDeckError, Result};

/// Current `web.json` schema version.
pub const WEB_CREDENTIALS_VERSION: u32 = 1;

/// Digits in a bootstrap code. Four, matching the phone-pairing overlay and
/// design turn 2's artboard 2b ("The overlay shows a four-digit code, good for
/// two minutes"). The entropy is small by design — see the module's rate-limiter
/// section for the three defences that make it acceptable.
pub const BOOTSTRAP_CODE_DIGITS: usize = 4;

/// How long a freshly minted bootstrap code stays valid (Q4: "~120s").
pub const BOOTSTRAP_CODE_TTL_MS: u64 = 120_000;

/// Wrong guesses, summed over every address, that burn a live bootstrap code.
/// The per-address limiter cannot see a distributed guessing attack; this can.
pub const BOOTSTRAP_CODE_MAX_FAILURES: u32 = 10;

/// Random bytes in a persistent token (256 bits).
pub const TOKEN_SECRET_BYTES: usize = 32;

/// Random bytes in a [`TokenId`]. The id is **not** derived from the token, so
/// holding an id tells an attacker nothing about the secret it names.
pub const TOKEN_ID_BYTES: usize = 8;

/// Failed attempts from one address before it is locked out. Artboard 2b's
/// footer counts down from this: "3 attempts left before this address is
/// rate-limited for 60s".
pub const RATE_LIMIT_MAX_FAILURES: u32 = 3;

/// How long a locked-out address stays locked out (artboard 2b: 60s).
pub const RATE_LIMIT_LOCKOUT_MS: u64 = 60_000;

/// Cap on tracked addresses, so a flood of spoofed sources cannot grow the
/// limiter's map without bound. See the module docs for what this costs.
pub const RATE_LIMIT_MAX_TRACKED_ADDRESSES: usize = 1024;

/// How many revoked tokens are kept as tombstones. A tombstone is what lets a
/// returning browser be told "your access was revoked" (artboard 2b's amber
/// screen) instead of the generic code-entry screen, and it is what stops a
/// stale `web.json` from resurrecting a revoked token. Past the cap the oldest
/// tombstones are dropped and that browser degrades to the code-entry screen —
/// a worse explanation, never a weaker refusal.
pub const REVOKED_TOMBSTONE_CAP: usize = 32;

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

/// A live bootstrap code: the digits the desktop overlay shows and the QR
/// encodes, plus when they stop working.
///
/// The digits are a secret for ~120 seconds. There is deliberately no `Display`
/// and the [`Debug`] implementation redacts them, so no formatting of any struct
/// that holds one can leak the code into a log. Reading it requires
/// [`BootstrapCode::reveal`].
#[derive(Clone, PartialEq, Eq)]
pub struct BootstrapCode {
    digits: String,
    expires_at_ms: u64,
}

impl std::fmt::Debug for BootstrapCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the digits: this code arrives in a URL fragment
        // specifically so that it stays out of every log (see module docs).
        f.debug_struct("BootstrapCode")
            .field("digits", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

impl BootstrapCode {
    /// The digits, for the one caller that must see them: the desktop overlay
    /// (and the QR payload built from it). Every other use is a bug.
    pub fn reveal(&self) -> &str {
        &self.digits
    }

    /// The [`Clock::now_millis`] reading after which this code is dead.
    pub fn expires_at_millis(&self) -> u64 {
        self.expires_at_ms
    }

    /// Whole seconds left before expiry, saturating at zero — the overlay's
    /// countdown, mirroring [`crate::remote::pairing::PairingSession::seconds_remaining`].
    pub fn seconds_remaining(&self, now_ms: u64) -> u64 {
        self.expires_at_ms.saturating_sub(now_ms) / 1000
    }

    /// Whether the code is still inside its validity window.
    pub fn is_live(&self, now_ms: u64) -> bool {
        now_ms < self.expires_at_ms
    }
}

/// A freshly issued persistent token, handed out exactly once by
/// [`CredentialStore::exchange_code`].
///
/// The host keeps only `SHA-256(secret)`, so this value cannot be recovered
/// later — the server must put it straight into the `Set-Cookie` header and
/// then drop it. Redacting [`Debug`] as with [`BootstrapCode`].
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken {
    id: TokenId,
    secret: String,
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the secret. The id is safe: it is independent random
        // bytes, not a derivation of the token.
        f.debug_struct("AccessToken")
            .field("id", &self.id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl AccessToken {
    /// The public, loggable identifier of this token's record.
    pub fn id(&self) -> &TokenId {
        &self.id
    }

    /// The cookie value. The single legitimate caller is the server building
    /// the `Set-Cookie` response for the exchange.
    pub fn reveal(&self) -> &str {
        &self.secret
    }
}

/// The public identifier of a persistent token's record: base64url of
/// [`TOKEN_ID_BYTES`] random bytes. Not a secret — it is what the desktop lists
/// and what [`CredentialStore::revoke`] takes — and safe to log.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TokenId(String);

impl TokenId {
    /// Mint a fresh random id.
    pub fn generate() -> Self {
        let mut bytes = [0u8; TOKEN_ID_BYTES];
        OsRng.fill_bytes(&mut bytes);
        TokenId(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// The wire/display form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TokenId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Failures
// ---------------------------------------------------------------------------

/// Why an authentication attempt was refused.
///
/// The variants are distinguishable to the *caller* so the browser can render
/// the right one of artboard 2b's three screens (see [`AccessScreen`]). What the
/// caller then reveals over the wire is its own decision — but note that no
/// variant here is an oracle an attacker can profit from:
///
/// * [`AuthFailure::CodeExpired`] is returned for **any** attempt once the
///   window has passed, whatever digits were presented, so it says nothing about
///   whether a guess was right.
/// * [`AuthFailure::CodeAlreadyUsed`] requires having presented the correct
///   digits, which an attacker who does not know them cannot do.
/// * [`AuthFailure::TokenRevoked`] likewise requires presenting a token the host
///   really did issue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthFailure {
    /// No bootstrap code is outstanding — nobody has run the palette command,
    /// or the overlay was dismissed.
    NoCodeOutstanding,
    /// A code is outstanding but the presented digits are not it.
    WrongCode,
    /// The outstanding code's window has passed, or it was burned by too many
    /// wrong guesses ([`BOOTSTRAP_CODE_MAX_FAILURES`]). Returned regardless of
    /// what was presented.
    CodeExpired,
    /// The correct digits, but this code has already been exchanged. Codes are
    /// single use.
    CodeAlreadyUsed,
    /// A token was presented that this host has no record of ever issuing — a
    /// forgery, or a cookie from a different host or a rotated credential.
    UnknownToken,
    /// A token this host did issue and has since revoked. Distinct from
    /// [`AuthFailure::UnknownToken`] because "someone withdrew your access" is a
    /// decision a person made, not a failure, and artboard 2b gives it its own
    /// amber screen.
    TokenRevoked {
        /// When the revocation happened, from the tombstone this host kept.
        ///
        /// Carried on the failure for the same reason
        /// [`AuthFailure::RateLimited`] carries `retry_after_ms`: artboard 2b's
        /// sentence — *"withdrew this browser's access **12s ago**"* — needs a
        /// number, and the only surface that knows it is the one refusing. A
        /// browser that has to guess writes a lie.
        revoked_at_unix_secs: u64,
    },
    /// This address has spent its attempt budget.
    RateLimited {
        /// Milliseconds until the address is allowed to try again — the value
        /// for [`crate::web::protocol::WireError::retry_after_ms`].
        retry_after_ms: u64,
    },
}

impl AuthFailure {
    /// A short, stable spelling for logs and the wire. Never includes anything
    /// secret.
    pub fn as_str(self) -> &'static str {
        match self {
            AuthFailure::NoCodeOutstanding => "no_code_outstanding",
            AuthFailure::WrongCode => "wrong_code",
            AuthFailure::CodeExpired => "code_expired",
            AuthFailure::CodeAlreadyUsed => "code_already_used",
            AuthFailure::UnknownToken => "unknown_token",
            AuthFailure::TokenRevoked { .. } => "token_revoked",
            AuthFailure::RateLimited { .. } => "rate_limited",
        }
    }

    /// Which of artboard 2b's screens the browser should show.
    pub fn screen(self) -> AccessScreen {
        match self {
            // Nothing was really "rejected" — this browser simply has no live
            // credential, which is the plain code-entry case.
            AuthFailure::NoCodeOutstanding | AuthFailure::UnknownToken => AccessScreen::CodeEntry,
            AuthFailure::WrongCode | AuthFailure::CodeExpired | AuthFailure::CodeAlreadyUsed => {
                AccessScreen::Rejected
            }
            AuthFailure::TokenRevoked { .. } => AccessScreen::Revoked,
            AuthFailure::RateLimited { .. } => AccessScreen::RateLimited,
        }
    }

    /// Whether this refusal was the rate limiter rather than the credential.
    pub fn is_rate_limited(self) -> bool {
        matches!(self, AuthFailure::RateLimited { .. })
    }
}

/// The browser-side screen a refusal maps to (design turn 2 §3, artboard 2b —
/// "three separate screens, because three different things went wrong").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessScreen {
    /// "Enter your code" — no credential, and nothing went wrong.
    CodeEntry,
    /// "That code did not work" — wrong, expired, or already spent.
    Rejected,
    /// "Access revoked" — amber, because someone decided this.
    Revoked,
    /// Refused by the per-address limiter; the browser waits it out.
    RateLimited,
}

// ---------------------------------------------------------------------------
// Persisted state
// ---------------------------------------------------------------------------

/// One browser's persistent access. Contains no secret: `token_sha256` is the
/// hash, which is why this type may derive [`Debug`] and be logged freely.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserToken {
    /// Public identifier, used to revoke this one browser.
    pub id: TokenId,
    /// base64url (no padding) of `SHA-256(token)`. See the module docs for why
    /// the hash and not the token.
    pub token_sha256: String,
    /// A human label for the desktop's list, e.g. a coarse user-agent name.
    /// Free text supplied by the browser, so treat it as untrusted for display.
    #[serde(default)]
    pub label: Option<String>,
    /// The peer address the **host observed** on the request that spent the
    /// bootstrap code, e.g. `192.168.2.20` (`remote-control-gk94`,
    /// `specs/WEB_INTERFACE.md` §6.5 R25).
    ///
    /// Never client-supplied, which is the whole distinction R12 draws between
    /// this and [`BrowserToken::label`]: the label is a claim and may say
    /// anything, the address is something the socket told us. It is the grant's
    /// address and is not refreshed as the browser moves — the honest reading
    /// is *"this is who I let in, and from where"*, which is the question the
    /// overlay's line exists to answer.
    ///
    /// `None` on a record issued before this field existed. The overlay then
    /// draws the row without an address rather than with a placeholder, the
    /// same way artboard 2f drops a seat fact it was told nothing about.
    #[serde(default)]
    pub address: Option<String>,
    /// Unix seconds when this token was issued.
    #[serde(default)]
    pub created_unix_secs: u64,
    /// Unix seconds when this token was last accepted. Persisted lazily (see
    /// [`LAST_SEEN_PERSIST_INTERVAL_SECS`]) so verifying a token on every
    /// request does not write a file on every request.
    #[serde(default)]
    pub last_seen_unix_secs: u64,
    /// Unix seconds when access was withdrawn, if it was. `Some` makes this
    /// record a tombstone rather than a credential.
    #[serde(default)]
    pub revoked_at_unix_secs: Option<u64>,
}

impl BrowserToken {
    /// Whether this record still grants access.
    pub fn is_active(&self) -> bool {
        self.revoked_at_unix_secs.is_none()
    }
}

/// The whole persisted web-credential state — `~/.flightdeck/web.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebCredentials {
    /// Schema version.
    pub version: u32,
    /// Every known browser, active and revoked, in issue order.
    #[serde(default)]
    pub tokens: Vec<BrowserToken>,
}

impl Default for WebCredentials {
    fn default() -> Self {
        WebCredentials {
            version: WEB_CREDENTIALS_VERSION,
            tokens: Vec::new(),
        }
    }
}

/// Only re-persist `last_seen_unix_secs` once it is this stale, so a busy
/// WebSocket does not turn every frame into a file write.
pub const LAST_SEEN_PERSIST_INTERVAL_SECS: u64 = 300;

/// The per-user credential path, `~/.flightdeck/web.json`. `None` when neither
/// `$HOME` nor `%USERPROFILE%` is set, in which case the caller runs without
/// persistence rather than failing — the same idiom as
/// [`crate::remote::state::remote_state_path`].
///
/// A sibling of `remote.json`, not a section inside it: the phone's identity and
/// the browser's access are separate concerns with separate lifecycles, and
/// rotating one must never risk rewriting the other.
pub fn web_credentials_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".flightdeck").join("web.json"))
}

/// Load and deserialize `web.json`.
pub fn load_web_credentials(fs: &dyn FileSystem, path: &Path) -> Result<WebCredentials> {
    let contents = fs.read_to_string(path).map_err(|e| {
        FlightDeckError::State(format!("failed to read web file {}: {e}", path.display()))
    })?;
    serde_json::from_str(&contents)
        .map_err(|e| FlightDeckError::State(format!("failed to parse web file: {e}")))
}

/// Serialize and write `web.json`, creating `~/.flightdeck/` if needed, then
/// hardening it to owner-only on Unix.
pub fn save_web_credentials(
    fs: &dyn FileSystem,
    path: &Path,
    state: &WebCredentials,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !fs.exists(parent) {
            fs.create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| FlightDeckError::State(format!("failed to serialize web credentials: {e}")))?;
    fs.write(path, &json)
        .map_err(|e| FlightDeckError::State(format!("failed to write web file: {e}")))?;
    harden_permissions(path);
    Ok(())
}

/// Best-effort owner-only (`0600`) hardening, matching
/// [`crate::remote::state`]: a direct `std::fs` call layered on the trait write,
/// because the [`FileSystem`] seam has no chmod. No-op off Unix, and silently
/// skipped when the path is not a real on-disk file (the in-memory test
/// filesystem).
///
/// The file holds token *hashes*, not tokens, so a readable `web.json` is not
/// immediately catastrophic — but it still lists every browser with access and
/// is exactly the kind of file that should not be world-readable.
#[cfg(unix)]
fn harden_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn harden_permissions(_path: &Path) {}

// ---------------------------------------------------------------------------
// Constant-time comparison
// ---------------------------------------------------------------------------

/// Compare two secrets without leaking where they diverge.
///
/// `a == b` on `&str`/`&[u8]` short-circuits at the first differing byte, so the
/// time it takes reveals the length of the matching prefix. Against a 4-digit
/// code that is enough to walk the answer out one digit at a time — this is the
/// exact bug the `subtle` dependency exists to prevent. **Never** compare a
/// bootstrap code or a token hash with `==`.
///
/// `subtle`'s slice implementation already folds the length check in, returning
/// a false [`subtle::Choice`] for mismatched lengths, so lengths (which are not
/// secret here — both are fixed-width) need no special handling.
fn secret_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

/// base64url (no padding) of `SHA-256(secret)` — the only form of a token this
/// host ever stores.
fn token_hash(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// A fresh uniformly-distributed decimal code of [`BOOTSTRAP_CODE_DIGITS`]
/// digits from the OS CSPRNG, rejection-sampled per digit so there is no modulo
/// bias. (`crate::remote::pairing` does the same for the phone's 4-digit code;
/// its helper is private to that module.)
fn random_code() -> String {
    let mut out = String::with_capacity(BOOTSTRAP_CODE_DIGITS);
    while out.len() < BOOTSTRAP_CODE_DIGITS {
        let mut buf = [0u8; 16];
        OsRng.fill_bytes(&mut buf);
        for byte in buf {
            // 250 is the largest multiple of 10 that fits in a u8; anything at
            // or above it would bias the low digits.
            if byte < 250 {
                out.push(char::from(b'0' + byte % 10));
                if out.len() == BOOTSTRAP_CODE_DIGITS {
                    break;
                }
            }
        }
    }
    out
}

/// A fresh 256-bit token secret, base64url (no padding) so it is a valid cookie
/// value with no escaping.
fn random_token_secret() -> String {
    let mut bytes = [0u8; TOKEN_SECRET_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Normalise what the browser sent before comparing: strip whitespace and the
/// separators a human might type. Operates only on attacker-supplied input, so
/// its data-dependent branches leak nothing about the real code.
fn normalize_code(presented: &str) -> String {
    presented
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .collect()
}

// ---------------------------------------------------------------------------
// Per-address rate limiter
// ---------------------------------------------------------------------------

/// One address's attempt budget.
#[derive(Clone, Copy, Debug, Default)]
struct AddressEntry {
    failures: u32,
    locked_until_ms: Option<u64>,
    last_failure_ms: u64,
}

/// Per-address failed-attempt budget with a fixed lockout (artboard 2b).
///
/// Keyed by whatever string the caller considers an address — the server passes
/// the peer IP without the port, so a NAT'd phone reconnecting on a new
/// ephemeral port keeps its budget instead of getting a fresh one. See the module
/// docs for what this does not protect against.
#[derive(Clone, Debug, Default)]
pub struct AddressLimiter {
    entries: HashMap<String, AddressEntry>,
}

impl AddressLimiter {
    /// An empty limiter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Milliseconds until `address` may try again, or `None` if it may try now.
    /// Read-only: a served lockout is cleared by [`AddressLimiter::check`].
    pub fn lockout_remaining_ms(&self, address: &str, now_ms: u64) -> Option<u64> {
        let entry = self.entries.get(address)?;
        entry
            .locked_until_ms
            .filter(|until| *until > now_ms)
            .map(|until| until - now_ms)
    }

    /// Attempts left before `address` is locked out. Zero while locked.
    pub fn attempts_remaining(&self, address: &str, now_ms: u64) -> u32 {
        if self.lockout_remaining_ms(address, now_ms).is_some() {
            return 0;
        }
        match self.entries.get(address) {
            // A lockout that has expired but not yet been cleared is a full
            // budget again, which is what the user is about to get.
            Some(entry) if entry.locked_until_ms.is_none() => {
                RATE_LIMIT_MAX_FAILURES.saturating_sub(entry.failures)
            }
            _ => RATE_LIMIT_MAX_FAILURES,
        }
    }

    /// Gate an attempt. `Err(retry_after_ms)` while locked; `Ok(())` otherwise,
    /// clearing a lockout that has now been served so the address starts again
    /// with a full budget.
    pub fn check(&mut self, address: &str, now_ms: u64) -> std::result::Result<(), u64> {
        let Some(entry) = self.entries.get_mut(address) else {
            return Ok(());
        };
        match entry.locked_until_ms {
            Some(until) if until > now_ms => Err(until - now_ms),
            Some(_) => {
                // Lockout served: fresh slate, exactly as if the address had
                // never failed.
                *entry = AddressEntry::default();
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Record a failed attempt, locking the address out once it has spent
    /// [`RATE_LIMIT_MAX_FAILURES`].
    pub fn record_failure(&mut self, address: &str, now_ms: u64) {
        self.prune(now_ms);
        let entry = self.entries.entry(address.to_string()).or_default();
        entry.failures = entry.failures.saturating_add(1);
        entry.last_failure_ms = now_ms;
        if entry.failures >= RATE_LIMIT_MAX_FAILURES {
            entry.locked_until_ms = Some(now_ms.saturating_add(RATE_LIMIT_LOCKOUT_MS));
        }
    }

    /// Forget `address` entirely — what a successful authentication does.
    pub fn reset(&mut self, address: &str) {
        self.entries.remove(address);
    }

    /// Drop entries that are neither locked nor recently active, once the map
    /// has grown past [`RATE_LIMIT_MAX_TRACKED_ADDRESSES`].
    fn prune(&mut self, now_ms: u64) {
        if self.entries.len() < RATE_LIMIT_MAX_TRACKED_ADDRESSES {
            return;
        }
        self.entries.retain(|_, entry| match entry.locked_until_ms {
            Some(until) => until > now_ms,
            None => now_ms.saturating_sub(entry.last_failure_ms) < RATE_LIMIT_LOCKOUT_MS,
        });
    }
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// The outstanding bootstrap code and its lifecycle state.
struct PendingCode {
    code: BootstrapCode,
    /// Set once the code has been exchanged. The record is kept (rather than
    /// dropped) so presenting the same digits again can be answered
    /// [`AuthFailure::CodeAlreadyUsed`] instead of the vaguer
    /// [`AuthFailure::WrongCode`].
    consumed: bool,
    /// Wrong guesses against this code, from every address combined.
    failures: u32,
}

/// The one object the web server asks about access.
///
/// Holds the persisted token set, the outstanding bootstrap code, and the
/// per-address limiter. Filesystem and clock arrive as [`Arc`] seams so the
/// store can live for the lifetime of the server task and still be driven by
/// fakes in tests.
///
/// Mutations that change who has access persist immediately — a revocation that
/// only existed in memory would come back on the next launch. Bookkeeping
/// (`last_seen`) is best-effort, as in [`crate::remote::state`].
pub struct CredentialStore {
    fs: Arc<dyn FileSystem + Send + Sync>,
    clock: Arc<dyn Clock + Send + Sync>,
    path: PathBuf,
    state: WebCredentials,
    pending: Option<PendingCode>,
    limiter: AddressLimiter,
    load_error: Option<String>,
    last_persist_error: Option<String>,
}

impl std::fmt::Debug for CredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Hand-written so the outstanding code can never be printed by a
        // `{:?}` on the store (or on anything holding it). `state` is safe: it
        // stores hashes.
        f.debug_struct("CredentialStore")
            .field("path", &self.path)
            .field("tokens", &self.state.tokens.len())
            .field("active_tokens", &self.active_tokens().count())
            .field("code_outstanding", &self.pending.is_some())
            .field("load_error", &self.load_error)
            .field("last_persist_error", &self.last_persist_error)
            .finish()
    }
}

impl CredentialStore {
    /// Open the store at `path`, loading `web.json` if it is there.
    ///
    /// Best-effort, like every other `~/.flightdeck` file: a missing or
    /// unreadable file simply means "no browser has access yet", and the reason
    /// is kept in [`CredentialStore::load_error`] so a caller can surface it
    /// rather than silently starting from empty.
    pub fn open(
        fs: Arc<dyn FileSystem + Send + Sync>,
        clock: Arc<dyn Clock + Send + Sync>,
        path: impl Into<PathBuf>,
    ) -> Self {
        let path = path.into();
        let (state, load_error) = match load_web_credentials(&*fs, &path) {
            Ok(state) => (state, None),
            // Distinguish "no file yet" (normal) from "there is a file and we
            // could not use it" (worth reporting).
            Err(_) if !fs.exists(&path) => (WebCredentials::default(), None),
            Err(e) => (WebCredentials::default(), Some(e.to_string())),
        };
        CredentialStore {
            fs,
            clock,
            path,
            state,
            pending: None,
            limiter: AddressLimiter::new(),
            load_error,
            last_persist_error: None,
        }
    }

    /// The file this store persists to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Why the existing `web.json` could not be loaded, if there was one and it
    /// failed.
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// Why the last best-effort persist failed, if it did.
    pub fn last_persist_error(&self) -> Option<&str> {
        self.last_persist_error.as_deref()
    }

    /// Every browser that currently has access, in issue order.
    ///
    /// The order is load-bearing for the access overlay: it numbers these rows
    /// and revokes the one a digit names, so the nth row and the nth active
    /// token have to be the same browser. Issue order gives that for free —
    /// a new grant appends and can never renumber a row already on screen.
    pub fn active_tokens(&self) -> impl Iterator<Item = &BrowserToken> + '_ {
        self.state.tokens.iter().filter(|t| t.is_active())
    }

    /// The host's own wall clock, for dating what [`CredentialStore::records`]
    /// remembers. Read through the store because it holds the [`Clock`] seam
    /// the tokens were stamped with, so an age can never be measured against a
    /// second, differently-faked clock.
    pub fn now_unix_secs(&self) -> u64 {
        self.clock.now_unix_secs()
    }

    /// Every record, tombstones included — for tests and for a desktop view
    /// that wants to show recently revoked browsers.
    pub fn records(&self) -> &[BrowserToken] {
        &self.state.tokens
    }

    /// Whether the record named by `id` still grants access **right now**.
    ///
    /// This is the question a *live* socket asks, and it is deliberately a
    /// different question from [`CredentialStore::verify_token`]: the socket
    /// never sees the secret again after the upgrade, and it must not have to.
    /// It kept the [`TokenId`] the upgrade handed it — a public identifier, safe
    /// to hold and safe to log — and asks about that.
    ///
    /// Because this store is the one authority on revocation, and because the
    /// web server consults it on every frame a socket sends (see
    /// `super::server::Shared::credential_is_active`), there is **no window**
    /// between a revocation landing here and that socket losing its powers.
    /// A design that cached "revoked" beside each socket would have one.
    ///
    /// Not constant time, and deliberately so: an id is not a secret, nobody
    /// authenticates with one, and every caller already holds a specific id it
    /// was given rather than a guess it is testing. An unknown id answers
    /// `false`, which is the safe direction — a record pruned by
    /// `prune_tombstones` grants nothing.
    pub fn is_token_active(&self, id: &TokenId) -> bool {
        self.state
            .tokens
            .iter()
            .any(|record| &record.id == id && record.is_active())
    }

    /// Write `web.json` now.
    pub fn save(&mut self) -> Result<()> {
        match save_web_credentials(&*self.fs, &self.path, &self.state) {
            Ok(()) => {
                self.last_persist_error = None;
                Ok(())
            }
            Err(e) => {
                self.last_persist_error = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// Re-read `web.json`, **without** resurrecting anything this process has
    /// already revoked.
    ///
    /// A plain re-read would be a hole: a stale file (an editor's backup
    /// restored, a synced home directory, a concurrently running second
    /// FlightDeck that saved an older token set) would hand back a token the
    /// user withdrew. So the tombstones held in memory are re-applied over
    /// whatever the file says — revocation is one-way within a process.
    pub fn reload(&mut self) -> Result<()> {
        let mut loaded = load_web_credentials(&*self.fs, &self.path)?;
        for known in self.state.tokens.iter().filter(|t| !t.is_active()) {
            match loaded.tokens.iter_mut().find(|t| t.id == known.id) {
                Some(existing) => existing.revoked_at_unix_secs = known.revoked_at_unix_secs,
                None => loaded.tokens.push(known.clone()),
            }
        }
        self.state = loaded;
        self.prune_tombstones();
        Ok(())
    }

    // -- bootstrap code ----------------------------------------------------

    /// Mint a fresh bootstrap code, replacing any outstanding one (pressing
    /// `Space` on the overlay for a new code, per artboard 2b's recovery step).
    ///
    /// Not persisted: the code is worth two minutes and should not survive a
    /// restart.
    pub fn mint_bootstrap_code(&mut self) -> BootstrapCode {
        let code = BootstrapCode {
            digits: random_code(),
            expires_at_ms: self
                .clock
                .now_millis()
                .saturating_add(BOOTSTRAP_CODE_TTL_MS),
        };
        self.pending = Some(PendingCode {
            code: code.clone(),
            consumed: false,
            failures: 0,
        });
        code
    }

    /// The outstanding code, if one is live (not expired, not burned, not yet
    /// exchanged) — what the overlay renders and the QR encodes.
    pub fn bootstrap_code(&self) -> Option<&BootstrapCode> {
        let pending = self.pending.as_ref()?;
        let live = !pending.consumed
            && pending.failures < BOOTSTRAP_CODE_MAX_FAILURES
            && pending.code.is_live(self.clock.now_millis());
        live.then_some(&pending.code)
    }

    /// Seconds left on the outstanding code, for the overlay countdown.
    pub fn bootstrap_seconds_remaining(&self) -> Option<u64> {
        let now = self.clock.now_millis();
        self.bootstrap_code().map(|c| c.seconds_remaining(now))
    }

    /// Drop the outstanding code — dismissing the overlay stops the code
    /// working, rather than leaving it live for its remaining seconds.
    ///
    /// Called from [`crate::web::access::WebAccess::on_close`], which applies
    /// it to the network state only: there the code was on screen and closing
    /// should take it back, while on loopback nothing was ever displayed and a
    /// browser launched a moment ago is very likely still starting up with it.
    pub fn clear_bootstrap_code(&mut self) {
        self.pending = None;
    }

    // -- exchange and verification ----------------------------------------

    /// Exchange a bootstrap code for a persistent token (Q4's one-time step).
    ///
    /// `address` is the peer's address for rate-limiting purposes; `label` is an
    /// optional human name for the desktop's list. On success the code is spent,
    /// the address's failure budget is reset, and `web.json` is written before
    /// returning — so a host that dies immediately afterwards still honours the
    /// cookie the browser is about to store.
    pub fn exchange_code(
        &mut self,
        address: &str,
        presented: &str,
        label: Option<&str>,
    ) -> std::result::Result<AccessToken, AuthFailure> {
        let now_ms = self.clock.now_millis();
        if let Err(retry_after_ms) = self.limiter.check(address, now_ms) {
            return Err(AuthFailure::RateLimited { retry_after_ms });
        }

        let outcome = self.classify_code(presented, now_ms);
        match outcome {
            Err(failure) => {
                self.limiter.record_failure(address, now_ms);
                if failure == AuthFailure::WrongCode {
                    // Count the miss against the code itself, so a distributed
                    // guessing attack burns the code even though no single
                    // address spends its budget.
                    if let Some(pending) = self.pending.as_mut() {
                        pending.failures = pending.failures.saturating_add(1);
                    }
                }
                Err(failure)
            }
            Ok(()) => {
                if let Some(pending) = self.pending.as_mut() {
                    pending.consumed = true;
                }
                self.limiter.reset(address);
                let token = self.issue_token(address, label);
                // Persist the new token before the caller hands out the cookie.
                let _ = self.save();
                Ok(token)
            }
        }
    }

    /// Decide what the presented digits are worth, without touching any state.
    ///
    /// Order matters and is deliberate: expiry is judged **before** the digits
    /// are looked at, so an expired code answers [`AuthFailure::CodeExpired`]
    /// for every input and cannot be used as a "was my guess right?" oracle.
    fn classify_code(&self, presented: &str, now_ms: u64) -> std::result::Result<(), AuthFailure> {
        let Some(pending) = self.pending.as_ref() else {
            return Err(AuthFailure::NoCodeOutstanding);
        };
        if !pending.code.is_live(now_ms) || pending.failures >= BOOTSTRAP_CODE_MAX_FAILURES {
            return Err(AuthFailure::CodeExpired);
        }
        let candidate = normalize_code(presented);
        // Constant-time: see `secret_eq`. `==` here would leak the code one
        // digit at a time.
        if !secret_eq(candidate.as_bytes(), pending.code.digits.as_bytes()) {
            return Err(AuthFailure::WrongCode);
        }
        if pending.consumed {
            return Err(AuthFailure::CodeAlreadyUsed);
        }
        Ok(())
    }

    /// Mint, record and return a persistent token.
    ///
    /// `address` is the peer the exchange came from — the same string the
    /// limiter was consulted about, which is `peer.ip()` off the socket and
    /// never a header a client could set.
    fn issue_token(&mut self, address: &str, label: Option<&str>) -> AccessToken {
        let secret = random_token_secret();
        let id = TokenId::generate();
        let now = self.clock.now_unix_secs();
        self.state.tokens.push(BrowserToken {
            id: id.clone(),
            token_sha256: token_hash(&secret),
            label: label.map(str::to_string),
            address: Some(address.to_string()),
            created_unix_secs: now,
            last_seen_unix_secs: now,
            revoked_at_unix_secs: None,
        });
        AccessToken { id, secret }
    }

    /// Verify a persistent token (the cookie on every request).
    ///
    /// Scans **every** record, active and revoked, without an early exit, so the
    /// time taken does not depend on where in the list a match sits.
    ///
    /// A revoked token deliberately does **not** spend the address's attempt
    /// budget: the overwhelmingly common case is the user's own browser
    /// presenting a cookie that was withdrawn from the desktop, and locking that
    /// browser out of the code-entry screen for a minute would punish the user
    /// for the host's own decision. A token the host never issued does spend it.
    pub fn verify_token(
        &mut self,
        address: &str,
        presented: &str,
    ) -> std::result::Result<TokenId, AuthFailure> {
        let now_ms = self.clock.now_millis();
        if let Err(retry_after_ms) = self.limiter.check(address, now_ms) {
            return Err(AuthFailure::RateLimited { retry_after_ms });
        }

        let hash = token_hash(presented);
        let mut matched: Option<(usize, bool)> = None;
        for (index, record) in self.state.tokens.iter().enumerate() {
            // No `break`: a short-circuiting scan tells an attacker how deep
            // in the list their guess landed. `secret_eq` is constant time.
            if secret_eq(hash.as_bytes(), record.token_sha256.as_bytes()) {
                matched = Some((index, record.is_active()));
            }
        }

        match matched {
            Some((index, true)) => {
                self.limiter.reset(address);
                self.touch(index);
                Ok(self.state.tokens[index].id.clone())
            }
            Some((index, false)) => Err(AuthFailure::TokenRevoked {
                // A record that is not active has a revocation time by
                // construction (`is_active` *is* `revoked_at_unix_secs.is_none`),
                // so this branch never has to invent one. The `unwrap_or` is
                // unreachable and deliberately degrades to "we do not know" —
                // `0` would be a fabricated 1970 timestamp, so the wire treats a
                // zero as no time at all (see `refusal_body`).
                revoked_at_unix_secs: self.state.tokens[index].revoked_at_unix_secs.unwrap_or(0),
            }),
            None => {
                self.limiter.record_failure(address, now_ms);
                Err(AuthFailure::UnknownToken)
            }
        }
    }

    /// Record that a token was just used, persisting only when the stored value
    /// has gone stale (see [`LAST_SEEN_PERSIST_INTERVAL_SECS`]).
    fn touch(&mut self, index: usize) {
        let now = self.clock.now_unix_secs();
        let stale = now.saturating_sub(self.state.tokens[index].last_seen_unix_secs)
            >= LAST_SEEN_PERSIST_INTERVAL_SECS;
        self.state.tokens[index].last_seen_unix_secs = now;
        if stale {
            // Bookkeeping only — a failed write must never fail a request.
            let _ = self.save();
        }
    }

    // -- revoke and rotate -------------------------------------------------

    /// Withdraw one browser's access, keeping a tombstone so that browser is
    /// told it was revoked rather than merely unrecognised.
    ///
    /// Returns whether a token was actually revoked. Persists immediately, and
    /// **surfaces a write failure** rather than swallowing it: a revocation the
    /// user believes happened but that does not survive a restart is a security
    /// bug, not a bookkeeping one. Either way the token is refused for the rest
    /// of this process's life.
    pub fn revoke(&mut self, id: &TokenId) -> Result<bool> {
        let now = self.clock.now_unix_secs();
        let mut revoked = false;
        for record in self.state.tokens.iter_mut() {
            if &record.id == id && record.is_active() {
                record.revoked_at_unix_secs = Some(now);
                revoked = true;
            }
        }
        if !revoked {
            return Ok(false);
        }
        self.prune_tombstones();
        self.save()?;
        Ok(true)
    }

    /// Withdraw every browser's access. Returns how many were active.
    pub fn revoke_all(&mut self) -> Result<usize> {
        let now = self.clock.now_unix_secs();
        let mut count = 0;
        for record in self.state.tokens.iter_mut() {
            if record.is_active() {
                record.revoked_at_unix_secs = Some(now);
                count += 1;
            }
        }
        if count == 0 {
            return Ok(0);
        }
        self.prune_tombstones();
        self.save()?;
        Ok(count)
    }

    /// Rotate the credential (D5's "rotates on one command"): revoke every
    /// token and mint a fresh bootstrap code, so every browser must come back
    /// through the code screen.
    ///
    /// The code is returned even if persisting the revocations failed, because
    /// the in-memory state is already authoritative for this process — but the
    /// error is surfaced so the caller can tell the user their rotation may not
    /// survive a restart.
    pub fn rotate(&mut self) -> (BootstrapCode, Option<FlightDeckError>) {
        let error = self.revoke_all().err();
        (self.mint_bootstrap_code(), error)
    }

    /// **Test-only seam (debug builds only).** Mint a bootstrap code with
    /// *known* digits instead of random ones, so an automated harness can
    /// exchange it without reading the desktop's screen.
    ///
    /// The Playwright end-to-end suite (`webui/e2e`, D15) has to drive the real
    /// `POST /auth/exchange` in a real browser, and it cannot: the persistent
    /// token is stored as a SHA-256 hash so no usable credential can be read off
    /// disk, and a random 4-digit code is only ever rendered into a TUI overlay
    /// on a PTY. This is the same shape of seam as `PairingSession::begin_with_hint`
    /// (the `FLIGHTDECK_REMOTE_AUTOPAIR` startup hook), for the same reason.
    ///
    /// What it deliberately is **not**:
    ///
    /// * Not a bypass. The returned code still has to be exchanged over the real
    ///   endpoint, still expires after [`BOOTSTRAP_CODE_TTL_MS`], is still single
    ///   use, and is still subject to both rate limiters. Nothing here mints a
    ///   token or accepts one.
    /// * Not present in a release build. The whole method is
    ///   `#[cfg(debug_assertions)]`, so `cargo build --release` — which is how
    ///   every shipped binary is produced (`dist-workspace.toml`,
    ///   `.github/workflows/release.yml`) — does not compile it at all. There is
    ///   no runtime flag, no config key and no feature to accidentally leave on.
    ///
    /// Returns `None` (minting nothing) unless `digits` is exactly
    /// [`BOOTSTRAP_CODE_DIGITS`] ASCII digits, so a typo cannot install a code
    /// the exchange endpoint would never match.
    #[cfg(debug_assertions)]
    pub fn mint_fixed_bootstrap_code(&mut self, digits: &str) -> Option<BootstrapCode> {
        if digits.len() != BOOTSTRAP_CODE_DIGITS || !digits.bytes().all(|b: u8| b.is_ascii_digit())
        {
            return None;
        }
        let code = BootstrapCode {
            digits: digits.to_string(),
            expires_at_ms: self
                .clock
                .now_millis()
                .saturating_add(BOOTSTRAP_CODE_TTL_MS),
        };
        self.pending = Some(PendingCode {
            code: code.clone(),
            consumed: false,
            failures: 0,
        });
        Some(code)
    }

    /// Keep the tombstone list bounded, dropping the oldest revocations first.
    fn prune_tombstones(&mut self) {
        let mut revoked: Vec<(u64, TokenId)> = self
            .state
            .tokens
            .iter()
            .filter_map(|t| t.revoked_at_unix_secs.map(|at| (at, t.id.clone())))
            .collect();
        if revoked.len() <= REVOKED_TOMBSTONE_CAP {
            return;
        }
        // Oldest first, then drop the excess from the front.
        revoked.sort();
        let doomed: std::collections::HashSet<TokenId> = revoked
            .into_iter()
            .rev()
            .skip(REVOKED_TOMBSTONE_CAP)
            .map(|(_, id)| id)
            .collect();
        self.state.tokens.retain(|t| !doomed.contains(&t.id));
    }

    // -- rate-limit reporting ---------------------------------------------

    /// Attempts left for `address` before it is locked out — artboard 2b's
    /// "3 attempts left before this address is rate-limited for 60s".
    pub fn attempts_remaining(&self, address: &str) -> u32 {
        self.limiter
            .attempts_remaining(address, self.clock.now_millis())
    }

    /// Milliseconds until `address` may try again, or `None` if it may now.
    pub fn lockout_remaining_ms(&self, address: &str) -> Option<u64> {
        self.limiter
            .lockout_remaining_ms(address, self.clock.now_millis())
    }
}

#[cfg(test)]
mod tests;
