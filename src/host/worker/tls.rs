//! The worker's HTTPS certificate: fetched from Tailscale, kept on disk,
//! renewed, and handed to the web server (SPECS §33).
//!
//! A browser needs `https://` with a certificate it trusts before it treats
//! FlightDeck Web as a secure context; Tailscale issues exactly that for the
//! machine's MagicDNS name (`tailscale cert`, Let's Encrypt). [`CertLifecycle`]
//! decides when to ask for one and what the server should present:
//!
//! * **Consent first.** Only when `[worker] web` and `[worker] https` are on,
//!   the worker listens on Tailscale, the tailnet's `CertDomains` is
//!   non-empty and holds this machine's MagicDNS name. An empty `CertDomains`
//!   means the tailnet admin has not enabled HTTPS, and then `tailscale cert`
//!   is never run: issuing a certificate publishes the machine's name in the
//!   public Certificate Transparency logs, and the admin turning HTTPS on is
//!   the consent to that.
//! * **Never on the loop.** `tailscale cert` can take a minute (an ACME round
//!   trip), so every fetch runs on whatever [`Spawn`] the worker was given (a
//!   thread in production) and its outcome is picked up by a later
//!   [`CertLifecycle::poll`]. A tick never waits for Tailscale.
//! * **Disk first.** On start the certificate saved last time
//!   (`~/.flightdeck/worker/tls/{cert,key}.pem`, the key 0600) is served at
//!   once when it is valid and unexpired, then refreshed.
//! * **Renewal.** Fetched again every [`RENEW_EVERY_MS`] (Tailscale hands
//!   back its cached certificate until renewal is due, so this is cheap) and
//!   at once when the MagicDNS name changes; retried after
//!   [`RETRY_AFTER_MS`] when a fetch fails.
//! * **Failure keeps HTTP.** A fetch that fails leaves plain HTTP (or the
//!   certificate already served) in place and records why, for `worker status`.
//!
//! Every side effect is injected (flightdeck-architecture-seams): the
//! Tailscale CLI ([`TailscaleProbe`]), the files ([`CertFiles`]), where a
//! fetch runs ([`Spawn`]) and the clock (each `poll` is told the time), so
//! the whole lifecycle runs in a unit test with fakes.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

use crate::remote::direct::net::TailscaleProbe;
use crate::web::cert::{certificate_not_after, format_date};

/// How often a certificate that was fetched is fetched again.
pub const RENEW_EVERY_MS: u64 = 24 * 60 * 60 * 1000;
/// How long after a failed (or refused) fetch the next attempt waits.
pub const RETRY_AFTER_MS: u64 = 30 * 60 * 1000;

/// Why HTTPS is not used while the tailnet has HTTPS certificates off.
pub const NOT_ENABLED: &str =
    "HTTPS certificates are not enabled for this tailnet (an admin can turn them on in the \
     Tailscale admin console, under DNS)";
/// Why HTTPS is not used while `[worker] https = false`.
pub const TURNED_OFF: &str = "turned off ([worker] https = false)";
/// Why HTTPS is not used when the worker does not listen on Tailscale.
pub const NOT_ON_TAILSCALE: &str = "the worker is not listening on Tailscale";
/// Why HTTPS is not used yet: the first check has not answered.
pub const CHECKING: &str = "checking Tailscale for a certificate";

/// Where the certificate is kept between runs.
pub trait CertFiles: Send {
    /// The saved `(certificate chain, key)` PEMs, if both are there.
    fn load(&self) -> Option<(String, String)>;
    /// Save both, the key readable by this account only.
    fn save(&self, cert_pem: &str, key_pem: &str) -> Result<(), String>;
}

/// [`CertFiles`] on disk: `cert.pem` and `key.pem` in a 0700 directory, the
/// key 0600, each written to a temporary file and renamed into place so a
/// crash never leaves half a key.
pub struct DiskCertFiles {
    dir: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}

impl DiskCertFiles {
    pub fn new(dir: PathBuf, cert: PathBuf, key: PathBuf) -> Self {
        DiskCertFiles { dir, cert, key }
    }

    fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
        use std::io::Write as _;
        let tmp = path.with_extension("pem.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        // A file left from an earlier, wider umask keeps its mode on open:
        // narrow it before anything secret is written.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    }
}

impl CertFiles for DiskCertFiles {
    fn load(&self) -> Option<(String, String)> {
        let cert = std::fs::read_to_string(&self.cert).ok()?;
        let key = std::fs::read_to_string(&self.key).ok()?;
        Some((cert, key))
    }

    fn save(&self, cert_pem: &str, key_pem: &str) -> Result<(), String> {
        let saved = (|| {
            std::fs::create_dir_all(&self.dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
            }
            Self::write_private(&self.key, key_pem)?;
            Self::write_private(&self.cert, cert_pem)
        })();
        saved.map_err(|e| {
            format!(
                "could not save the certificate in {}: {e}",
                self.dir.display()
            )
        })
    }
}

/// Runs a fetch somewhere other than the worker's loop.
pub type Spawn = Box<dyn Fn(Box<dyn FnOnce() + Send>) + Send>;

/// [`Spawn`] on a fresh, named thread: production's.
pub fn thread_spawner() -> Spawn {
    Box::new(|job| {
        let _ = std::thread::Builder::new()
            .name("worker-tls-cert".to_string())
            .spawn(job);
    })
}

/// What the lifecycle needs from the outside world.
pub struct CertSetup {
    /// Asks Tailscale for the status (`CertDomains`) and the certificate.
    pub probe: Arc<dyn TailscaleProbe>,
    pub files: Box<dyn CertFiles>,
    pub spawn: Spawn,
}

/// The certificate the server presents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedCert {
    pub cert_pem: String,
    pub key_pem: String,
    /// The name it was fetched for; `None` for one loaded from disk at start
    /// that no fetch has confirmed yet.
    pub domain: Option<String>,
    /// Its notAfter (unix seconds), when it could be read.
    pub not_after: Option<u64>,
}

/// What [`CertLifecycle::poll`] wants the server to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CertChange {
    /// Present [`CertLifecycle::current`].
    Load,
    /// Present nothing: refuse TLS, serve plain HTTP.
    Clear,
}

/// HTTPS as `worker status` and `worker pair` describe it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CertReport {
    /// Whether a certificate is being served.
    pub https: bool,
    /// The name it is for, when known.
    pub domain: Option<String>,
    /// Why HTTPS is not available, when it is not.
    pub reason: Option<String>,
    /// The served certificate's notAfter (unix seconds), when known.
    pub expires: Option<u64>,
}

/// What one fetch found.
#[derive(Debug)]
enum FetchOutcome {
    Issued {
        domain: String,
        cert: String,
        key: String,
    },
    /// `CertDomains` is empty: HTTPS is off for the tailnet.
    NotEnabled,
    Failed(String),
}

/// One fetch, start to end, on the thread [`Spawn`] gave it.
fn fetch(probe: &dyn TailscaleProbe) -> FetchOutcome {
    let Some(status) = probe.status() else {
        return FetchOutcome::Failed("the tailscale CLI did not answer".to_string());
    };
    if status.cert_domains.is_empty() {
        return FetchOutcome::NotEnabled;
    }
    let Some(name) = status.magic_dns else {
        return FetchOutcome::Failed("MagicDNS is off for this machine".to_string());
    };
    if !status
        .cert_domains
        .iter()
        .any(|domain| domain.eq_ignore_ascii_case(&name))
    {
        return FetchOutcome::Failed(format!(
            "{name} is not one of this tailnet's certificate domains"
        ));
    }
    match probe.cert(&name) {
        Ok((cert, key)) => FetchOutcome::Issued {
            domain: name,
            cert,
            key,
        },
        Err(e) => FetchOutcome::Failed(format!("tailscale cert failed: {e}")),
    }
}

/// The worker's HTTPS certificate, from first fetch to renewal.
pub struct CertLifecycle {
    /// `None` when HTTPS cannot be used at all, with [`Self::off_reason`].
    setup: Option<CertSetup>,
    off_reason: Option<&'static str>,
    started: bool,
    loaded: Option<LoadedCert>,
    /// Why HTTPS is not available right now (or why the last fetch failed).
    reason: Option<String>,
    in_flight: Option<Receiver<FetchOutcome>>,
    /// When the next fetch is due (host clock, ms).
    next_fetch_ms: u64,
    log: Vec<String>,
}

impl CertLifecycle {
    /// The lifecycle for a worker whose config says `web` and `https`, with
    /// `setup` when it listens on Tailscale (else `None`).
    pub fn new(web: bool, https: bool, setup: Option<CertSetup>) -> Self {
        let off_reason = if !web {
            Some("the browser app is off ([worker] web = false)")
        } else if !https {
            Some(TURNED_OFF)
        } else if setup.is_none() {
            Some(NOT_ON_TAILSCALE)
        } else {
            None
        };
        CertLifecycle {
            setup: if off_reason.is_none() { setup } else { None },
            off_reason,
            started: false,
            loaded: None,
            reason: None,
            in_flight: None,
            next_fetch_ms: 0,
            log: Vec::new(),
        }
    }

    /// A lifecycle that never serves HTTPS (the tests' default).
    pub fn off() -> Self {
        CertLifecycle::new(true, true, None)
    }

    /// The certificate the server should present.
    pub fn current(&self) -> Option<&LoadedCert> {
        self.loaded.as_ref()
    }

    /// Lines for the worker log since the last call.
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    /// The server could not use the certificate after all: stop presenting
    /// it, say why, and try again later.
    pub fn rejected(&mut self, why: String, now_ms: u64) {
        self.loaded = None;
        self.log
            .push(format!("HTTPS off: the certificate was refused: {why}"));
        self.reason = Some(format!("the certificate could not be used: {why}"));
        self.next_fetch_ms = now_ms.saturating_add(RETRY_AFTER_MS);
    }

    /// HTTPS as it stands.
    pub fn report(&self) -> CertReport {
        match (&self.loaded, self.off_reason) {
            (_, Some(off)) => CertReport {
                reason: Some(off.to_string()),
                ..CertReport::default()
            },
            (Some(cert), None) => CertReport {
                https: true,
                domain: cert.domain.clone(),
                reason: None,
                expires: cert.not_after,
            },
            (None, None) => CertReport {
                reason: Some(self.reason.clone().unwrap_or_else(|| CHECKING.to_string())),
                ..CertReport::default()
            },
        }
    }

    /// One turn of the lifecycle at host time `now_ms` (unix ms), with the
    /// MagicDNS name the listener last saw. Never blocks: a fetch is started
    /// on the [`Spawn`] and its outcome read on a later turn. Returns what the
    /// server should now present, when that changed.
    pub fn poll(&mut self, now_ms: u64, magic_dns: Option<&str>) -> Option<CertChange> {
        self.setup.as_ref()?;
        let mut change = None;
        if !self.started {
            self.started = true;
            change = self.load_saved(now_ms);
        }
        if let Some(outcome) = self.take_outcome() {
            if let Some(c) = self.apply(outcome, now_ms) {
                change = Some(c);
            }
        }
        // An expired certificate is worse than none: a browser refuses it
        // outright, where plain HTTP at least loads.
        let expired = self
            .loaded
            .as_ref()
            .and_then(|c| c.not_after)
            .is_some_and(|not_after| not_after <= now_ms / 1000);
        if expired {
            self.loaded = None;
            self.reason = Some("the certificate expired".to_string());
            self.log
                .push("HTTPS off: the certificate expired; fetching a new one".to_string());
            self.next_fetch_ms = now_ms;
            change = Some(CertChange::Clear);
        }
        // A new MagicDNS name needs a certificate of its own, now.
        let renamed = match (
            magic_dns,
            self.loaded.as_ref().and_then(|c| c.domain.as_deref()),
        ) {
            (Some(now), Some(was)) => !now.eq_ignore_ascii_case(was),
            _ => false,
        };
        if renamed && self.in_flight.is_none() && self.next_fetch_ms > now_ms {
            self.log.push(format!(
                "the MagicDNS name is now {}: fetching a certificate for it",
                magic_dns.unwrap_or_default()
            ));
            self.next_fetch_ms = now_ms;
        }
        if self.in_flight.is_none() && now_ms >= self.next_fetch_ms {
            self.start_fetch();
        }
        change
    }

    /// Serve the certificate saved last time, if it is still good.
    fn load_saved(&mut self, now_ms: u64) -> Option<CertChange> {
        let (cert, key) = self.setup.as_ref()?.files.load()?;
        let not_after = certificate_not_after(&cert);
        let usable = match not_after {
            Some(not_after) if not_after > now_ms / 1000 => {
                crate::web::server::check_certificate(&cert, &key)
            }
            Some(_) => Err("it has expired".to_string()),
            None => Err("its expiry cannot be read".to_string()),
        };
        match usable {
            Ok(()) => {
                self.log.push(format!(
                    "serving the saved HTTPS certificate (until {})",
                    not_after.map_or_else(|| "an unknown date".to_string(), format_date)
                ));
                self.loaded = Some(LoadedCert {
                    cert_pem: cert,
                    key_pem: key,
                    domain: None,
                    not_after,
                });
                Some(CertChange::Load)
            }
            Err(why) => {
                self.log
                    .push(format!("not serving the saved HTTPS certificate: {why}"));
                None
            }
        }
    }

    fn take_outcome(&mut self) -> Option<FetchOutcome> {
        let rx = self.in_flight.as_ref()?;
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                FetchOutcome::Failed("the certificate fetch stopped without an answer".to_string())
            }
        };
        self.in_flight = None;
        Some(outcome)
    }

    fn apply(&mut self, outcome: FetchOutcome, now_ms: u64) -> Option<CertChange> {
        match outcome {
            FetchOutcome::Issued { domain, cert, key } => {
                if let Err(why) = crate::web::server::check_certificate(&cert, &key) {
                    self.fail(
                        format!("the certificate Tailscale returned could not be used: {why}"),
                        now_ms,
                    );
                    return None;
                }
                self.next_fetch_ms = now_ms.saturating_add(RENEW_EVERY_MS);
                self.reason = None;
                let unchanged = self
                    .loaded
                    .as_ref()
                    .is_some_and(|c| c.cert_pem == cert && c.key_pem == key);
                if unchanged {
                    // Tailscale handed back the certificate already served:
                    // nothing to swap, but now its name is known.
                    if let Some(loaded) = self.loaded.as_mut() {
                        loaded.domain = Some(domain);
                    }
                    return None;
                }
                if let Some(setup) = self.setup.as_ref() {
                    if let Err(e) = setup.files.save(&cert, &key) {
                        // Served anyway: only the next start's head start is lost.
                        self.log.push(e);
                    }
                }
                let not_after = certificate_not_after(&cert);
                self.log.push(format!(
                    "serving HTTPS for {domain} (certificate until {})",
                    not_after.map_or_else(|| "an unknown date".to_string(), format_date)
                ));
                self.loaded = Some(LoadedCert {
                    cert_pem: cert,
                    key_pem: key,
                    domain: Some(domain),
                    not_after,
                });
                Some(CertChange::Load)
            }
            FetchOutcome::NotEnabled => {
                self.next_fetch_ms = now_ms.saturating_add(RETRY_AFTER_MS);
                let was_serving = self.loaded.take().is_some();
                if self.reason.as_deref() != Some(NOT_ENABLED) {
                    self.log.push(format!("no HTTPS: {NOT_ENABLED}"));
                }
                self.reason = Some(NOT_ENABLED.to_string());
                // The admin turned HTTPS off: the consent is gone, so the
                // certificate (still valid as it may be) is not served.
                was_serving.then_some(CertChange::Clear)
            }
            FetchOutcome::Failed(why) => {
                self.fail(why, now_ms);
                None
            }
        }
    }

    /// A fetch failed: keep whatever is served, say why, retry later.
    fn fail(&mut self, why: String, now_ms: u64) {
        self.next_fetch_ms = now_ms.saturating_add(RETRY_AFTER_MS);
        if self.reason.as_deref() != Some(why.as_str()) {
            let keeping = if self.loaded.is_some() {
                "keeping the certificate already served"
            } else {
                "serving plain HTTP"
            };
            self.log
                .push(format!("no new HTTPS certificate ({keeping}): {why}"));
        }
        self.reason = Some(why);
    }

    fn start_fetch(&mut self) {
        let Some(setup) = self.setup.as_ref() else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let probe = Arc::clone(&setup.probe);
        (setup.spawn)(Box::new(move || {
            let _ = tx.send(fetch(&*probe));
        }));
        self.in_flight = Some(rx);
    }
}

#[cfg(all(test, unix))]
mod tests;
