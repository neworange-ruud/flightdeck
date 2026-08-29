//! The desktop access overlay: the surface that hands a browser its way in
//! (`specs/WEB_INTERFACE.md` D5, D10, Q1, Q4, Q7; design `2a`, both states).
//!
//! ## What this module is for
//!
//! [`crate::web::credentials`] can mint a bootstrap code and
//! [`crate::web::server`] can exchange one, but between them sat nothing: no
//! production caller ever minted a code and no surface ever displayed one, so a
//! release binary served a page nobody could authenticate against. This module
//! is the missing middle. It owns the overlay's *state* — which of artboard
//! 2a's two states is showing, which address is selected, whether the code is
//! revealed — and every decision that state implies. The renderer
//! ([`crate::tui::render::draw_web_access_overlay`]) draws a snapshot of it and
//! decides nothing; the event loop performs the side effects it asks for and
//! decides nothing either.
//!
//! ## The two states (Q1)
//!
//! ```text
//!   State A — local only (the default, D5)      State B — network enabled
//!   ────────────────────────────────────        ─────────────────────────
//!   Enter  open in browser, authenticated       QR: http://<lan-ip>:port/#<code>
//!   c      copy a URL for a second browser      the code beside it, large
//!   n      allow other devices  ──────────────▶ ↑↓ pick which address to publish
//!   s      stop the server                      Space  a fresh code
//!   Esc    close                                r      hide code and QR
//!                                               1-9    revoke that one browser
//!                                               x      revoke every browser
//!                                               l      back to local only
//!   never shows a code                          Esc    close
//! ```
//!
//! State A is the common case and it **never draws a credential**. That is not
//! a simplification, it is the point: on loopback the QR is useless (it encodes
//! an address that only resolves on the machine that printed it) and the code is
//! a shoulder-surfing hazard bought for nothing. So the local path spends the
//! code invisibly — see the next section — and only the network path, where the
//! credential genuinely has to cross a gap between two devices, puts one on
//! screen.
//!
//! ## "Open in browser, already authenticated" — how, and what it costs
//!
//! Enter builds `http://127.0.0.1:<port>/#<code>` and hands it to the platform
//! browser launcher. The **fragment** is what makes this safe enough to ship,
//! and it is the same convention Q4 chose for the phone: a fragment is never
//! sent to the server, so the code cannot land in the request line, an access
//! log, a proxy log, a `Referer`, or a crash report. The SPA reads
//! `location.hash`, POSTs it to `/auth/exchange` in a body, and strips the
//! fragment from history.
//!
//! There is one residual exposure and it is stated rather than hidden: the URL
//! is an **argv of the launcher process**, so another user on a shared machine
//! who runs `ps` during the second or two the launcher lives can read the code.
//! We accept it, for these reasons and no others:
//!
//! * There is no portable way to hand a URL to the default browser without it
//!   appearing in a command line — not on macOS, Linux or Windows. The
//!   alternative is not a better transport, it is no feature.
//! * The exposure is bounded by the credential itself: the code is single use,
//!   dies after [`crate::web::credentials::BOOTSTRAP_CODE_TTL_MS`], and the
//!   browser normally spends it within a second of the spawn.
//! * The obvious "safer" alternative — letting any loopback request in without
//!   a code — is **strictly worse**. It is not a two-second window, it is a
//!   permanent standing grant to every other local user, and D5 deliberately
//!   does not have one.
//!
//! What this module will not do: log the code. `debuglog` near auth logs a
//! [`crate::web::credentials::TokenId`] and never a secret, and
//! [`BootstrapCode`] has a redacting `Debug` and no `Display` so a stray format
//! cannot leak one. `reveal()` is called in exactly two places here — building
//! the URL the launcher is handed, and building the view the overlay draws —
//! and nowhere else.
//!
//! ## The code is minted here, and re-minted on request
//!
//! [`WebAccess::open`] mints one code when the overlay opens, so the acceptance
//! path ("start the interface, a code exists") never depends on the user
//! pressing anything. After that the code is left to live and die honestly: it
//! is **not** silently re-minted behind a countdown that never reaches zero,
//! because a countdown that lies is worse than no countdown. When it expires the
//! overlay says so and `Space` mints another ([`WebAccess::mint`]) without the
//! server restarting. The one exception is [`WebAccess::live_code_for_launch`],
//! which re-mints if the outstanding code is gone at the moment the user asks
//! for a browser — pressing Enter must open a working browser, not report an
//! expiry the user was never shown.
//!
//! ## Seams (flightdeck-architecture-seams)
//!
//! Nothing here performs a side effect. Minting goes through a
//! [`CredentialStore`] the caller lends (which owns the [`FileSystem`] and
//! [`Clock`] seams), interface enumeration through
//! [`InterfaceEnumerator`], and everything the event loop must actually *do* —
//! spawn a browser, write the clipboard, rebind the listener — leaves as an
//! [`AccessOutcome`] for it to perform. That is what lets the whole state
//! machine be tested at a table without a socket, a browser or a real network.
//!
//! [`FileSystem`]: crate::contracts::FileSystem
//! [`Clock`]: crate::contracts::Clock

#[cfg(test)]
mod tests;

use std::net::{Ipv4Addr, SocketAddr};

use crate::web::credentials::{BootstrapCode, CredentialStore};
use crate::web::interfaces::{InterfaceEnumerator, NetworkInterface};
use crate::web::server::BindExposure;

/// The loopback host the local-only state publishes.
///
/// Written out rather than taken from the bound address because a host that
/// configured `bind = "0.0.0.0"` is bound to a wildcard, and `http://0.0.0.0:…`
/// is not a URL a browser can be handed.
pub const LOOPBACK_HOST: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// Which of artboard 2a's two states the overlay is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// State A: bound to loopback, no credential on screen (D5's default).
    LocalOnly,
    /// State B: bound to a routable address, publishing a QR and a code.
    Network,
}

/// One row of the address picker (Q1 addition 2, "the address is chosen, not
/// guessed"). A view type: the description is already resolved, so the renderer
/// never has to know what an [`crate::web::interfaces::InterfaceClass`] is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressRow {
    /// The interface name as the OS reports it (`en0`, `wlan0`, `Wi-Fi`).
    pub name: String,
    /// Its IPv4 address, as it will appear in the published URL.
    pub address: String,
    /// The one-line description, or `None` when the name matched no known
    /// pattern — the picker then shows the bare name rather than a guess.
    pub description: Option<&'static str>,
}

/// One browser that currently holds access, as artboard 2a State B draws it:
/// `● 192.168.2.20 · Safari on iOS · 14m`.
///
/// Every field is a fact the host already stored, and each is presented at the
/// standing it has. [`BrowserRow::address`] is the host's own observation off
/// the socket; [`BrowserRow::browser`] is the browser's claim about itself,
/// coarsened and stripped of control characters and **never parsed** — R12's
/// rule, applied to the second surface that shows a user-agent. Nothing here is
/// split back out of a joined string, because a user-agent may contain any
/// separator you might split on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserRow {
    /// The digit that revokes this row (`1`..=`9`), or `None` past the ninth —
    /// a tenth browser is still listed and still revoked by `x`, it just has no
    /// key of its own. See [`WebAccess::network_key`] for why digits and not a
    /// second `↑↓` picker.
    pub key: Option<char>,
    /// The address the host observed when it granted this credential, or `None`
    /// for a record issued before the address was stored. The row then goes
    /// without it rather than printing a placeholder.
    pub address: Option<String>,
    /// What the browser said it was, coarsened to `Safari on iOS` and capped.
    /// `None` when it said nothing usable.
    pub browser: Option<String>,
    /// How long ago this browser was let in, on the **host's** clock — the
    /// store's own [`crate::contracts::Clock`], the one that stamped the
    /// record, so the two numbers are always from the same source.
    pub granted_secs_ago: u64,
}

/// How many listed browsers get a digit of their own. Nine, because `0` would
/// have to mean "the tenth" and a key whose label is off by one is worse than
/// no key.
const MAX_KEYED_BROWSERS: usize = 9;

/// One key press the access overlay understands, already lifted out of
/// crossterm's [`KeyEvent`] by the event loop.
///
/// Named keys are variants rather than the characters a terminal happens to
/// send for them, so the two footers can be matched exhaustively and a future
/// binding cannot be added without every arm being revisited.
///
/// [`KeyEvent`]: crossterm::event::KeyEvent
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessKey {
    Enter,
    Esc,
    Space,
    Up,
    Down,
    Char(char),
}

/// What the event loop must do as a result of a key press. Every variant is a
/// side effect this module deliberately does not perform itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessOutcome {
    /// The key did nothing (unbound, or bound only in the other state).
    Ignored,
    /// The overlay handled it entirely; redraw and carry on.
    Handled,
    /// Close the overlay. The code is dropped with it — a dismissed overlay
    /// should not leave a live credential behind for its remaining seconds.
    Close,
    /// Hand this URL to the platform browser (State A's `Enter`). Carries a
    /// live code in its fragment; see the module docs on what that costs.
    OpenBrowser(String),
    /// Put this URL on the clipboard (State A's `c`). Also code-bearing, so it
    /// authenticates the *second* browser it is pasted into.
    CopyUrl(String),
    /// Rebind the listener to `0.0.0.0` and reopen in State B (`n`).
    EnableNetwork,
    /// Rebind the listener back to loopback and reopen in State A (`l`).
    BackToLocalOnly,
    /// Stop the server (State A's `s`).
    StopServer,
    /// Credentials were withdrawn in the store (State B's `x`), and the live
    /// sockets holding them must now be closed.
    ///
    /// The store side is already done by the time this is returned — this
    /// module owns the credential decision — but the *eviction* is not, because
    /// the sockets live behind [`crate::web::server::WebServerHandle`], which
    /// only the event loop holds. Without it a revoked browser keeps typing
    /// into every terminal until it happens to reconnect, which is what
    /// `remote-control-glmt` was.
    ///
    /// It carries no token ids on purpose. The store is the one authority on
    /// which credential is live, every socket consults it about its own token,
    /// and a list copied out of it here would be a second answer that could
    /// disagree with the first.
    Revoked,
}

/// Render-ready snapshot of the access overlay, rebuilt every tick from
/// [`WebAccess`] and the live [`CredentialStore`] so the countdown moves without
/// the renderer touching any credential logic.
///
/// Mirrors [`crate::tui::render::RemotePairing`] in shape and in purpose: the
/// renderer receives facts, never a store and never a decision.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebAccessView {
    /// Which state is drawn.
    pub mode: Option<AccessMode>,
    /// The bound socket address, verbatim (`127.0.0.1:7420`, `0.0.0.0:7420`).
    pub bound: String,
    /// The one-line consequence of the current binding, in the host's words.
    pub exposure_line: String,
    /// The base URL, with **no** credential in it — what the URL row shows.
    pub url: String,
    /// The code, in large type beside the QR. `Some` only in State B, only
    /// while revealed, and only while a code is actually live.
    pub code: Option<String>,
    /// State B with the code and QR deliberately hidden (`r`, Q1 mitigation 1).
    pub code_hidden: bool,
    /// State B with no live code left: it expired, was spent, or was burned by
    /// wrong guesses. `Space` mints another.
    pub code_expired: bool,
    /// QR half-block art rows, black-on-white. Empty whenever the code is not
    /// being drawn, so hiding the code hides the QR by construction.
    pub qr_rows: Vec<String>,
    /// Width of the QR art in terminal cells.
    pub qr_width: usize,
    /// Seconds left on the live code, for the countdown.
    pub seconds_remaining: Option<u64>,
    /// The address picker's rows (State B only; empty in State A).
    pub addresses: Vec<AddressRow>,
    /// Index of the selected row in [`WebAccessView::addresses`].
    pub selected_address: Option<usize>,
    /// Every browser that currently holds access, in the store's issue order —
    /// the header line counts them and the rows name them.
    ///
    /// One list rather than a count beside it: the count *is* `browsers.len()`,
    /// and a separate field would be a second answer to "how many" that could
    /// disagree with the rows underneath it.
    pub browsers: Vec<BrowserRow>,
    /// The result of the last action, shown for one glance and then replaced.
    pub notice: Option<String>,
    /// The footer legend: `(key, label)` in the order the artboard lists them.
    pub keys: Vec<(&'static str, &'static str)>,
}

/// The access overlay's live state.
///
/// Held by the event loop for as long as the overlay is open. It borrows a
/// [`CredentialStore`] for the moments it needs one and holds none, because the
/// store is shared with the server thread behind a mutex and this type must
/// never be the thing holding that lock across a render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAccess {
    mode: AccessMode,
    /// Where the listener actually is. Read from the handle, not from config,
    /// so the overlay cannot describe a binding the server does not have.
    bound: SocketAddr,
    exposure: BindExposure,
    addresses: Vec<NetworkInterface>,
    selected: usize,
    /// Whether the code and QR are drawn. `true` on open, matching artboard 2a
    /// (`r hide code`); see `specs/WEB_INTERFACE.md` Q7 for why the artboard
    /// wins over Q7's "hidden behind a reveal" proposal.
    revealed: bool,
    notice: Option<String>,
}

impl WebAccess {
    /// Open the overlay for a server that is already listening, minting the
    /// first bootstrap code.
    ///
    /// The state is derived from the *binding*, not from a preference: a host
    /// whose `config.toml` already says `bind = "0.0.0.0"` opened a routable
    /// socket, so it opens in State B with the picker populated. Anything
    /// loopback opens in State A.
    pub fn open(
        store: &mut CredentialStore,
        enumerator: &dyn InterfaceEnumerator,
        bound: SocketAddr,
        exposure: BindExposure,
    ) -> WebAccess {
        let mode = match exposure {
            BindExposure::Loopback => AccessMode::LocalOnly,
            BindExposure::Routable => AccessMode::Network,
        };
        let addresses = match mode {
            AccessMode::LocalOnly => Vec::new(),
            AccessMode::Network => enumerator.enumerate(),
        };
        store.mint_bootstrap_code();
        WebAccess {
            mode,
            bound,
            exposure,
            addresses,
            selected: 0,
            revealed: true,
            notice: None,
        }
    }

    /// Which state is showing.
    pub fn mode(&self) -> AccessMode {
        self.mode
    }

    /// The bound address the overlay is describing.
    pub fn bound(&self) -> SocketAddr {
        self.bound
    }

    /// The selected interface, if the picker has any rows.
    pub fn selected_address(&self) -> Option<&NetworkInterface> {
        self.addresses.get(self.selected)
    }

    /// Replace the last notice (or clear it). The event loop uses this to report
    /// the outcome of the side effect it just performed, in its own words —
    /// nothing here invents a result it did not witness.
    pub fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice;
    }

    /// Mint a fresh bootstrap code, replacing any outstanding one (`Space`).
    pub fn mint(&self, store: &mut CredentialStore) -> BootstrapCode {
        store.mint_bootstrap_code()
    }

    /// D5's "rotates on one command": revoke every browser's token *and* mint a
    /// fresh code, so everyone must come back through the code screen. Returns
    /// how many browsers were locked out, plus any persistence error — the
    /// in-memory revocation still stands, but a caller must be able to say the
    /// rotation might not survive a restart.
    ///
    /// **This is the credential half only.** A browser that is attached right
    /// now keeps its socket until somebody closes it, which is why `x` returns
    /// [`AccessOutcome::Revoked`] rather than [`AccessOutcome::Handled`].
    ///
    /// It is all-or-nothing **on purpose**, and stayed that way through
    /// `remote-control-gk94`: D5 asks for one command that locks *everyone*
    /// out, and redefining the key the artboard draws would have taken that
    /// command away. [`WebAccess::revoke_one`] is the addition beside it, on
    /// the numbered rows, and it withdraws one credential through the same
    /// per-token machinery.
    pub fn rotate(&self, store: &mut CredentialStore) -> (usize, Option<String>) {
        let active = store.active_tokens().count();
        let (_code, error) = store.rotate();
        (active, error.map(|e| e.to_string()))
    }

    /// Withdraw **one** browser's access: the `index`th row of
    /// [`WebAccessView::browsers`], which is the `index`th active token
    /// (`remote-control-gk94`, §6.5 R25).
    ///
    /// Returns how the revoked browser was described on screen, plus any
    /// persistence error, or `None` when there is no such row — a digit pressed
    /// past the end of the list must do nothing, not revoke the last one.
    ///
    /// The row is rebuilt here rather than remembered from the last render so
    /// the list a key acts on is the list the store has *now*. Issue order is
    /// what makes that safe: a browser that authenticated since the last draw
    /// appends, so it cannot renumber a row the user is looking at.
    ///
    /// Like [`WebAccess::rotate`] this is the credential half only; the sockets
    /// are closed by the event loop, which is why the caller returns
    /// [`AccessOutcome::Revoked`].
    pub fn revoke_one(
        &self,
        store: &mut CredentialStore,
        index: usize,
    ) -> Option<(String, Option<String>)> {
        let described = describe_browser(browser_rows(store).get(index)?);
        let id = store.active_tokens().nth(index)?.id.clone();
        let error = store.revoke(&id).err().map(|e| e.to_string());
        Some((described, error))
    }

    /// A code that is live *right now*, minting one if the outstanding code has
    /// expired or been spent.
    ///
    /// Only the launch paths use this. The displayed code is deliberately not
    /// refreshed this way (see the module docs): a countdown that silently
    /// restarts is a countdown that lies. But `Enter` and `c` are a user asking
    /// for a working browser, and answering "your invisible code expired" would
    /// be reporting a state State A never showed them.
    pub fn live_code_for_launch(&self, store: &mut CredentialStore) -> BootstrapCode {
        match store.bootstrap_code() {
            Some(code) => code.clone(),
            None => store.mint_bootstrap_code(),
        }
    }

    /// The base URL, carrying no credential: `http://127.0.0.1:7420` in State A,
    /// `http://<selected-ip>:7420` in State B.
    ///
    /// `None` in State B when the picker found no interface at all — there is
    /// then no routable address to publish, and saying so beats printing
    /// `http://0.0.0.0:7420`, which no browser can reach.
    pub fn base_url(&self) -> Option<String> {
        let port = self.bound.port();
        match self.mode {
            AccessMode::LocalOnly => Some(format!("http://{LOOPBACK_HOST}:{port}")),
            AccessMode::Network => self
                .selected_address()
                .map(|iface| format!("http://{}:{}", iface.address, port)),
        }
    }

    /// The base URL with a bootstrap code in its fragment — what a browser is
    /// actually handed, and what the QR encodes (Q1 addition 1).
    ///
    /// `reveal()` is called here because this string *is* the credential in
    /// transit; see the module docs for the fragment's guarantees and its one
    /// residual exposure.
    pub fn authenticated_url(&self, code: &BootstrapCode) -> Option<String> {
        self.base_url()
            .map(|base| format!("{base}/#{}", code.reveal()))
    }

    /// Handle one key, per artboard 2a's two footers. Returns what the event
    /// loop must do; everything that can be decided without a side effect is
    /// already done by the time this returns.
    ///
    /// The two states have deliberately disjoint key sets — the artboard draws
    /// two different footers — so a key bound only in the other state returns
    /// [`AccessOutcome::Ignored`] rather than doing something the footer never
    /// promised.
    pub fn handle_key(&mut self, key: AccessKey, store: &mut CredentialStore) -> AccessOutcome {
        // Esc closes from either state, so it is decided before the split.
        if matches!(key, AccessKey::Esc) {
            return AccessOutcome::Close;
        }
        match self.mode {
            AccessMode::LocalOnly => self.local_key(key, store),
            AccessMode::Network => self.network_key(key, store),
        }
    }

    /// State A's footer: `Enter open · c copy · n network access · s stop
    /// server`. Nothing here ever draws or returns a visible credential — the
    /// two code-bearing outcomes carry it in a URL fragment.
    fn local_key(&mut self, key: AccessKey, store: &mut CredentialStore) -> AccessOutcome {
        match key {
            AccessKey::Enter => match self.launch_url(store) {
                Some(url) => AccessOutcome::OpenBrowser(url),
                None => AccessOutcome::Ignored,
            },
            AccessKey::Char('c') => match self.launch_url(store) {
                Some(url) => AccessOutcome::CopyUrl(url),
                None => AccessOutcome::Ignored,
            },
            AccessKey::Char('n') => AccessOutcome::EnableNetwork,
            AccessKey::Char('s') => AccessOutcome::StopServer,
            AccessKey::Esc | AccessKey::Space | AccessKey::Up | AccessKey::Down => {
                AccessOutcome::Ignored
            }
            AccessKey::Char(_) => AccessOutcome::Ignored,
        }
    }

    /// State B's footer: `↑↓ address · Space new code · r hide · x revoke ·
    /// l local only`.
    fn network_key(&mut self, key: AccessKey, store: &mut CredentialStore) -> AccessOutcome {
        match key {
            AccessKey::Up => {
                self.select_prev();
                AccessOutcome::Handled
            }
            AccessKey::Down => {
                self.select_next();
                AccessOutcome::Handled
            }
            AccessKey::Space => {
                self.mint(store);
                // A fresh code is worth nothing behind a reveal the user forgot
                // they had toggled, so `Space` also brings it back on screen —
                // the same gesture that hid it is one keystroke away.
                self.revealed = true;
                self.notice = Some("New code — the previous one no longer works.".to_string());
                AccessOutcome::Handled
            }
            AccessKey::Char('r') => {
                self.revealed = !self.revealed;
                self.notice = None;
                AccessOutcome::Handled
            }
            AccessKey::Char('x') => {
                let (revoked, error) = self.rotate(store);
                self.revealed = true;
                self.notice = Some(rotate_notice(revoked, error.as_deref()));
                // Not `Handled`: the notice above claims those browsers are out,
                // and until the event loop closes their sockets that claim is
                // false. The outcome is what makes the claim true.
                AccessOutcome::Revoked
            }
            AccessKey::Char('l') => AccessOutcome::BackToLocalOnly,
            // `1`..`9` revoke the browser on that numbered row.
            //
            // Digits rather than a second `↑↓` picker because `↑↓` already
            // belongs to the address list, and a second list would need a focus
            // concept the overlay does not have — two lists, one pair of arrows
            // and an invisible mode is exactly how a revoke lands on the wrong
            // browser. Each row prints its own digit, so the affordance is on
            // screen next to the thing it acts on rather than only in the
            // footer, and `x` keeps meaning what D5 and 2a's footer say it
            // means: everyone out.
            AccessKey::Char(d) if d.is_ascii_digit() && d != '0' => {
                let index = (d as usize) - ('1' as usize);
                match self.revoke_one(store, index) {
                    // A digit past the end of the list revokes nothing. Silence
                    // rather than a notice: there was no row there to act on,
                    // and reporting a refusal for a key the overlay never
                    // offered would be noise.
                    None => AccessOutcome::Ignored,
                    Some((described, error)) => {
                        self.notice = Some(revoke_one_notice(&described, error.as_deref()));
                        AccessOutcome::Revoked
                    }
                }
            }
            AccessKey::Enter | AccessKey::Esc => AccessOutcome::Ignored,
            AccessKey::Char(_) => AccessOutcome::Ignored,
        }
    }

    /// The code-bearing URL both of State A's launch paths hand out, or `None`
    /// when there is no address to build one from.
    fn launch_url(&self, store: &mut CredentialStore) -> Option<String> {
        let code = self.live_code_for_launch(store);
        self.authenticated_url(&code)
    }

    /// Move the picker's selection up, stopping at the top rather than wrapping
    /// — the list is short and a wrap makes "which one am I on" harder to read.
    fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        self.notice = None;
    }

    /// Move the picker's selection down, stopping at the last row.
    fn select_next(&mut self) {
        let last = self.addresses.len().saturating_sub(1);
        self.selected = (self.selected + 1).min(last);
        self.notice = None;
    }

    /// Dismissing the overlay: put the credential away, but only if it was
    /// ever taken out.
    ///
    /// In **State B** the code and the QR were on screen, so closing revokes
    /// them — otherwise a bystander who read the code over your shoulder keeps
    /// a working credential for up to two minutes after you have hidden it,
    /// which is the opposite of what closing an overlay ought to mean.
    ///
    /// In **State A** nothing was ever displayed, and something is very likely
    /// still using the code: `Enter` hands a browser a URL and browsers take a
    /// moment to start. Clearing here would make "Open in browser" fail for
    /// anyone who dismissed the overlay while their browser was launching — a
    /// refusal bought for no security at all, because there was nothing on
    /// screen to overhear.
    pub fn on_close(&self, store: &mut CredentialStore) {
        match self.mode {
            AccessMode::LocalOnly => {}
            AccessMode::Network => store.clear_bootstrap_code(),
        }
    }

    /// Rebuild after the event loop rebound the listener, keeping the overlay
    /// open. A fresh code is minted because the address the old one would have
    /// been carried to has just changed.
    pub fn rebind(
        &mut self,
        store: &mut CredentialStore,
        enumerator: &dyn InterfaceEnumerator,
        bound: SocketAddr,
        exposure: BindExposure,
        notice: Option<String>,
    ) {
        let reopened = WebAccess::open(store, enumerator, bound, exposure);
        self.mode = reopened.mode;
        self.bound = reopened.bound;
        self.exposure = reopened.exposure;
        self.addresses = reopened.addresses;
        self.selected = 0;
        self.revealed = true;
        self.notice = notice;
    }

    /// The render-ready snapshot for this frame.
    ///
    /// `qr` is injected rather than computed here so this module keeps no
    /// dependency on the QR encoder, and so a test can assert *that* a payload
    /// was offered for encoding without asserting anything about half-block
    /// art. It is called at most once, and only when a code is actually being
    /// drawn — which is what makes "hidden" hide the QR by construction rather
    /// than by a second check the renderer could forget.
    pub fn view(
        &self,
        store: &CredentialStore,
        qr: impl FnOnce(&str) -> Option<(Vec<String>, usize)>,
    ) -> WebAccessView {
        let live = store.bootstrap_code();
        let seconds = store.bootstrap_seconds_remaining();
        let drawing_code = matches!(self.mode, AccessMode::Network) && self.revealed;

        let (code, qr_rows, qr_width) = match (drawing_code, live) {
            (true, Some(live)) => {
                let (rows, width) = self
                    .authenticated_url(live)
                    .and_then(|url| qr(&url))
                    .unwrap_or_default();
                (Some(live.reveal().to_string()), rows, width)
            }
            (true, None) | (false, _) => (None, Vec::new(), 0),
        };

        WebAccessView {
            mode: Some(self.mode),
            bound: self.bound.to_string(),
            exposure_line: exposure_line(self.exposure).to_string(),
            url: self
                .base_url()
                .unwrap_or_else(|| "(no routable address on this host)".to_string()),
            code,
            code_hidden: matches!(self.mode, AccessMode::Network) && !self.revealed,
            code_expired: matches!(self.mode, AccessMode::Network)
                && self.revealed
                && live.is_none(),
            qr_rows,
            qr_width,
            seconds_remaining: seconds,
            addresses: self
                .addresses
                .iter()
                .map(|iface| AddressRow {
                    name: iface.name.clone(),
                    address: iface.address.to_string(),
                    description: iface.description(),
                })
                .collect(),
            selected_address: (!self.addresses.is_empty()).then_some(self.selected),
            browsers: browser_rows(store),
            notice: self.notice.clone(),
            keys: keys_for(self.mode),
        }
    }
}

/// The host's one-line description of what the current binding means. D5's
/// warning, in the words the artboard draws.
fn exposure_line(exposure: BindExposure) -> &'static str {
    match exposure {
        BindExposure::Loopback => "loopback only — nothing off this machine can reach it",
        BindExposure::Routable => "reachable by anyone on this network who has the code",
    }
}

/// The footer legend for a state, in artboard 2a's order.
fn keys_for(mode: AccessMode) -> Vec<(&'static str, &'static str)> {
    match mode {
        AccessMode::LocalOnly => vec![
            ("Enter", "open"),
            ("c", "copy"),
            ("n", "network access"),
            ("s", "stop server"),
            ("Esc", "close"),
        ],
        AccessMode::Network => vec![
            ("↑↓", "address"),
            ("Space", "new code"),
            ("r", "hide"),
            // 2a's footer, byte for byte. `x` still locks everyone out, which
            // is what D5 asks for and what this label has always meant.
            //
            // `1-9` is deliberately **not** here. It belongs to the browser
            // list — every row prints its own digit and the header above them
            // says what the digits do — and that is where it has to be said:
            // the list is an echoed tier that a short terminal drops, and a
            // footer entry for keys whose rows are not on screen would be
            // pointing at nothing. The legend also has to survive at 100
            // columns, where a seventh pair pushes `Esc close` off the end.
            ("x", "revoke"),
            ("l", "local only"),
            ("Esc", "close"),
        ],
    }
}

/// The overlay's list of who holds access, built from the store and nothing
/// else.
///
/// Read through [`CredentialStore::active_tokens`] so the rows are in the same
/// issue order the digit keys index into — the nth row and the nth active token
/// are the same browser by construction, not by two places agreeing.
fn browser_rows(store: &CredentialStore) -> Vec<BrowserRow> {
    let now = store.now_unix_secs();
    store
        .active_tokens()
        .enumerate()
        .map(|(index, token)| BrowserRow {
            key: (index < MAX_KEYED_BROWSERS)
                .then(|| char::from_digit(index as u32 + 1, 10))
                .flatten(),
            address: token.address.clone(),
            browser: token.label.as_deref().and_then(browser_label),
            // `saturating_sub` rather than a signed difference: a record stamped
            // in the future (a clock that moved backwards) is drawn as `0s`,
            // never as a negative age — the same refusal to print a fabricated
            // duration that R12 records for the seat rows.
            granted_secs_ago: now.saturating_sub(token.created_unix_secs),
        })
        .collect()
}

/// The browser's own claim about itself, reduced to something safe to draw.
///
/// The stored label is whatever the browser POSTed to `/auth/exchange` — in
/// practice the raw `navigator.userAgent`, in principle any bytes at all. It
/// goes through the *same* reduction the viewer chip uses
/// ([`crate::web::server::coarse_user_agent`]) so both surfaces say `Safari on
/// iOS` about the same browser, and a claim that reduces to nothing falls back
/// to the sanitised, capped text rather than being dropped: a custom client
/// that named itself honestly should still be identifiable.
///
/// `None` when nothing usable survives, and the row is then drawn without a
/// browser rather than with a guess.
fn browser_label(raw: &str) -> Option<String> {
    let coarse = crate::web::server::coarse_user_agent(raw);
    let text = if coarse.is_empty() {
        crate::web::server::sanitize_label(raw)
    } else {
        coarse
    };
    let text = crate::web::server::truncate_chars(&text, crate::web::server::MAX_LABEL_CHARS);
    (!text.is_empty()).then_some(text)
}

/// `14m` — a duration in the artboard's compact shape, rounded down so it never
/// claims more time than has passed.
///
/// Deliberately not the browser's `agoLabel` shape (`4m ago`): this sits at the
/// end of a row of facts, where the trailing `ago` reads as prose rather than
/// as the fourth fact.
pub fn age_label(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// How one browser reads on screen, for the notice that reports its removal.
///
/// The same facts the row drew, joined — and joining is the safe direction, the
/// one R12 keeps: the parts were never merged in storage, so nothing here has
/// to be split apart again.
fn describe_browser(row: &BrowserRow) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(address) = &row.address {
        parts.push(address.clone());
    }
    if let Some(browser) = &row.browser {
        parts.push(browser.clone());
    }
    if parts.is_empty() {
        // A record from before the address was stored, by a browser that
        // claimed nothing usable. It is still a real credential and it was
        // still revoked, so the notice says so without naming what it cannot.
        return "That browser".to_string();
    }
    parts.join(" · ")
}

/// What a digit did. Names the browser it locked out, because "1 browser
/// revoked" is the answer the user could already have worked out and the whole
/// point of the numbered rows is that they can be told apart.
fn revoke_one_notice(described: &str, error: Option<&str>) -> String {
    let head = format!("{described} is locked out — its access was revoked.");
    match error {
        None => head,
        Some(error) => {
            format!("{head} Could not save the revocation ({error}); it may not survive a restart.")
        }
    }
}

/// What `x` did, said without overclaiming. A failed persist does not undo the
/// revocation in this process, so the line reports both facts rather than
/// picking one.
fn rotate_notice(revoked: usize, error: Option<&str>) -> String {
    let head = match revoked {
        0 => "No browser held access — new code issued.".to_string(),
        1 => "1 browser revoked — new code issued.".to_string(),
        n => format!("{n} browsers revoked — new code issued."),
    };
    match error {
        None => head,
        Some(error) => {
            format!("{head} Could not save the revocation ({error}); it may not survive a restart.")
        }
    }
}
