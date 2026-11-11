//! The certificate lifecycle against a scripted Tailscale, in-memory files,
//! an inline "thread" and a clock the test sets.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::*;
use crate::remote::direct::net::TailscaleStatus;

const CERT: &str = include_str!("../../../../tests/fixtures/tls/cert.pem");
const KEY: &str = include_str!("../../../../tests/fixtures/tls/key.pem");
/// The fixture's notAfter (`tests/fixtures/tls/README.md`).
const NOT_AFTER: u64 = 4_947_990_185;
const NAME: &str = "box.tail1234.ts.net";
/// 2027-01-15, unix ms: well inside the fixture's validity.
const NOW: u64 = 1_800_000_000_000;

/// A Tailscale whose answers the test sets, counting what was asked.
struct ScriptedProbe {
    status: Mutex<Option<TailscaleStatus>>,
    cert: Mutex<Result<(String, String), String>>,
    status_calls: AtomicUsize,
    cert_calls: AtomicUsize,
}

impl ScriptedProbe {
    fn new(cert_domains: &[&str]) -> Arc<Self> {
        Arc::new(ScriptedProbe {
            status: Mutex::new(Some(status(NAME, cert_domains))),
            cert: Mutex::new(Ok((CERT.to_string(), KEY.to_string()))),
            status_calls: AtomicUsize::new(0),
            cert_calls: AtomicUsize::new(0),
        })
    }

    fn certs_asked(&self) -> usize {
        self.cert_calls.load(Ordering::SeqCst)
    }
}

fn status(name: &str, cert_domains: &[&str]) -> TailscaleStatus {
    TailscaleStatus {
        magic_dns: Some(name.to_string()),
        ips: vec!["100.64.0.1".parse().unwrap()],
        cert_domains: cert_domains.iter().map(|d| d.to_string()).collect(),
    }
}

impl TailscaleProbe for ScriptedProbe {
    fn status(&self) -> Option<TailscaleStatus> {
        self.status_calls.fetch_add(1, Ordering::SeqCst);
        self.status.lock().unwrap().clone()
    }

    fn cert(&self, domain: &str) -> Result<(String, String), String> {
        self.cert_calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            Some(domain.to_string()),
            self.status
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|s| s.magic_dns.clone()),
            "a certificate is only ever asked for this machine's name"
        );
        self.cert.lock().unwrap().clone()
    }
}

/// Files in memory, shared with the test.
#[derive(Clone, Default)]
struct MemoryFiles(Arc<Mutex<Option<(String, String)>>>);

impl CertFiles for MemoryFiles {
    fn load(&self) -> Option<(String, String)> {
        self.0.lock().unwrap().clone()
    }
    fn save(&self, cert_pem: &str, key_pem: &str) -> Result<(), String> {
        *self.0.lock().unwrap() = Some((cert_pem.to_string(), key_pem.to_string()));
        Ok(())
    }
}

/// A lifecycle whose fetches run inline (their outcome is read on the next
/// poll, exactly as a thread's would be).
fn lifecycle(probe: &Arc<ScriptedProbe>, files: &MemoryFiles) -> CertLifecycle {
    CertLifecycle::new(
        true,
        true,
        Some(CertSetup {
            probe: Arc::clone(probe) as Arc<dyn TailscaleProbe>,
            files: Box::new(files.clone()),
            spawn: Box::new(|job| job()),
        }),
    )
}

#[test]
fn a_certificate_is_fetched_saved_and_served() {
    let probe = ScriptedProbe::new(&[NAME]);
    let files = MemoryFiles::default();
    let mut certs = lifecycle(&probe, &files);
    assert_eq!(certs.report().reason.as_deref(), Some(CHECKING));

    // The first turn only starts the fetch: the loop never waits for it.
    assert_eq!(certs.poll(NOW, Some(NAME)), None);
    assert_eq!(probe.certs_asked(), 1);
    assert_eq!(certs.poll(NOW + 25, Some(NAME)), Some(CertChange::Load));

    let current = certs.current().expect("served");
    assert_eq!(current.domain.as_deref(), Some(NAME));
    assert_eq!(current.not_after, Some(NOT_AFTER));
    assert_eq!(
        files.load(),
        Some((CERT.to_string(), KEY.to_string())),
        "kept for the next start"
    );
    assert_eq!(
        certs.report(),
        CertReport {
            https: true,
            domain: Some(NAME.to_string()),
            reason: None,
            expires: Some(NOT_AFTER),
        }
    );
    assert!(certs
        .take_log()
        .iter()
        .any(|l| l == &format!("serving HTTPS for {NAME} (certificate until 2126-10-18)")));
    // Nothing more until renewal is due.
    assert_eq!(certs.poll(NOW + 60_000, Some(NAME)), None);
    assert_eq!(probe.certs_asked(), 1);
}

/// No `CertDomains`: the admin has not enabled HTTPS, which is the consent
/// to a public certificate — so `tailscale cert` is never run.
#[test]
fn no_certificate_is_asked_for_while_the_tailnet_has_https_off() {
    let probe = ScriptedProbe::new(&[]);
    let files = MemoryFiles::default();
    let mut certs = lifecycle(&probe, &files);
    for at in [NOW, NOW + 1, NOW + RETRY_AFTER_MS, NOW + RETRY_AFTER_MS + 1] {
        assert_eq!(certs.poll(at, Some(NAME)), None);
    }
    assert_eq!(probe.certs_asked(), 0);
    assert!(
        probe.status_calls.load(Ordering::SeqCst) >= 2,
        "it keeps checking"
    );
    assert!(certs.current().is_none());
    assert_eq!(certs.report().reason.as_deref(), Some(NOT_ENABLED));
    assert_eq!(files.load(), None);
}

/// A name outside `CertDomains` is not asked for either.
#[test]
fn a_name_outside_the_certificate_domains_is_not_asked_for() {
    let probe = ScriptedProbe::new(&["other.tail1234.ts.net"]);
    let mut certs = lifecycle(&probe, &MemoryFiles::default());
    certs.poll(NOW, Some(NAME));
    certs.poll(NOW + 1, Some(NAME));
    assert_eq!(probe.certs_asked(), 0);
    assert!(certs
        .report()
        .reason
        .unwrap()
        .contains("not one of this tailnet's certificate domains"));
}

#[test]
fn https_off_in_the_config_never_asks_tailscale_anything() {
    let probe = ScriptedProbe::new(&[NAME]);
    let files = MemoryFiles::default();
    files.save(CERT, KEY).unwrap();
    for (web, https) in [(true, false), (false, true)] {
        let mut certs = CertLifecycle::new(
            web,
            https,
            Some(CertSetup {
                probe: Arc::clone(&probe) as Arc<dyn TailscaleProbe>,
                files: Box::new(files.clone()),
                spawn: Box::new(|job| job()),
            }),
        );
        assert_eq!(certs.poll(NOW, Some(NAME)), None);
        assert_eq!(certs.poll(NOW + 1, Some(NAME)), None);
        assert!(certs.current().is_none(), "not even the saved one");
        assert!(!certs.report().https);
    }
    assert_eq!(probe.status_calls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.certs_asked(), 0);
    let mut certs = CertLifecycle::new(true, false, None);
    certs.poll(NOW, None);
    assert_eq!(certs.report().reason.as_deref(), Some(TURNED_OFF));
    assert_eq!(
        CertLifecycle::off().report().reason.as_deref(),
        Some(NOT_ON_TAILSCALE)
    );
}

/// A failed fetch keeps plain HTTP, says why, and is retried later — not on
/// every tick.
#[test]
fn a_failed_fetch_keeps_http_with_a_reason_and_retries_later() {
    let probe = ScriptedProbe::new(&[NAME]);
    *probe.cert.lock().unwrap() = Err("rate limited by Let's Encrypt".to_string());
    let mut certs = lifecycle(&probe, &MemoryFiles::default());
    certs.poll(NOW, Some(NAME));
    assert_eq!(certs.poll(NOW + 1, Some(NAME)), None);
    assert!(certs.current().is_none());
    let report = certs.report();
    assert!(!report.https);
    assert_eq!(
        report.reason.as_deref(),
        Some("tailscale cert failed: rate limited by Let's Encrypt")
    );
    assert!(certs
        .take_log()
        .iter()
        .any(|l| l.contains("serving plain HTTP")));

    certs.poll(NOW + RETRY_AFTER_MS - 1, Some(NAME));
    assert_eq!(probe.certs_asked(), 1, "not before the retry is due");

    *probe.cert.lock().unwrap() = Ok((CERT.to_string(), KEY.to_string()));
    certs.poll(NOW + 1 + RETRY_AFTER_MS, Some(NAME));
    assert_eq!(probe.certs_asked(), 2);
    assert_eq!(
        certs.poll(NOW + 2 + RETRY_AFTER_MS, Some(NAME)),
        Some(CertChange::Load)
    );
    assert!(certs.report().https);
}

/// A certificate that does not parse, or whose key is not its own, is never
/// served.
#[test]
fn an_unusable_certificate_is_not_served() {
    let probe = ScriptedProbe::new(&[NAME]);
    *probe.cert.lock().unwrap() = Ok((CERT.to_string(), "not a key".to_string()));
    let files = MemoryFiles::default();
    let mut certs = lifecycle(&probe, &files);
    certs.poll(NOW, Some(NAME));
    assert_eq!(certs.poll(NOW + 1, Some(NAME)), None);
    assert!(certs.current().is_none());
    assert!(certs
        .report()
        .reason
        .unwrap()
        .starts_with("the certificate Tailscale returned could not be used"));
    assert_eq!(files.load(), None, "nothing unusable is saved");
}

/// Every 24 hours the certificate is fetched again, and a new one swaps in.
#[test]
fn renewal_every_day_swaps_in_a_new_certificate() {
    let probe = ScriptedProbe::new(&[NAME]);
    let files = MemoryFiles::default();
    let mut certs = lifecycle(&probe, &files);
    certs.poll(NOW, Some(NAME));
    assert_eq!(certs.poll(NOW + 1, Some(NAME)), Some(CertChange::Load));

    // Tailscale renewed: a different chain (the fixture twice stands in).
    let renewed = format!("{CERT}{CERT}");
    *probe.cert.lock().unwrap() = Ok((renewed.clone(), KEY.to_string()));
    certs.poll(NOW + RENEW_EVERY_MS, Some(NAME));
    assert_eq!(probe.certs_asked(), 1, "not a millisecond early");
    certs.poll(NOW + 1 + RENEW_EVERY_MS, Some(NAME));
    assert_eq!(probe.certs_asked(), 2);
    assert_eq!(
        certs.poll(NOW + 2 + RENEW_EVERY_MS, Some(NAME)),
        Some(CertChange::Load)
    );
    assert_eq!(certs.current().unwrap().cert_pem, renewed);
    assert_eq!(files.load().unwrap().0, renewed);

    // The same certificate handed back again: no swap.
    certs.poll(NOW + 3 + 2 * RENEW_EVERY_MS, Some(NAME));
    assert_eq!(certs.poll(NOW + 4 + 2 * RENEW_EVERY_MS, Some(NAME)), None);
    assert_eq!(probe.certs_asked(), 3);
}

/// The certificate saved last time is served at once on start, before
/// Tailscale has answered anything.
#[test]
fn the_saved_certificate_is_served_at_start() {
    let probe = ScriptedProbe::new(&[NAME]);
    let files = MemoryFiles::default();
    files.save(CERT, KEY).unwrap();
    let mut certs = lifecycle(&probe, &files);
    // Tailscale is slow to answer: nothing comes back from the fetch yet.
    *probe.status.lock().unwrap() = None;
    assert_eq!(certs.poll(NOW, Some(NAME)), Some(CertChange::Load));
    assert_eq!(certs.current().unwrap().domain, None, "not confirmed yet");
    assert!(certs.report().https);
    // The CLI did not answer: the saved certificate stays.
    assert_eq!(certs.poll(NOW + 1, Some(NAME)), None);
    assert!(certs.report().https);

    // Next time Tailscale hands back the same certificate: the name is now
    // known, and nothing is swapped.
    *probe.status.lock().unwrap() = Some(status(NAME, &[NAME]));
    certs.poll(NOW + 2 + RETRY_AFTER_MS, Some(NAME));
    assert_eq!(certs.poll(NOW + 3 + RETRY_AFTER_MS, Some(NAME)), None);
    assert_eq!(certs.current().unwrap().domain.as_deref(), Some(NAME));
}

/// An expired saved certificate is not served; a served one that expires is
/// taken down and a new one fetched.
#[test]
fn an_expired_certificate_is_never_served() {
    let after = (NOT_AFTER + 1) * 1000;
    let probe = ScriptedProbe::new(&[NAME]);
    *probe.cert.lock().unwrap() = Err("offline".to_string());
    let files = MemoryFiles::default();
    files.save(CERT, KEY).unwrap();
    let mut certs = lifecycle(&probe, &files);
    assert_eq!(certs.poll(after, Some(NAME)), None);
    assert!(certs.current().is_none());
    assert!(certs.take_log().iter().any(|l| l.contains("expired")));

    let probe = ScriptedProbe::new(&[NAME]);
    let mut certs = lifecycle(&probe, &MemoryFiles::default());
    certs.poll(NOW, Some(NAME));
    assert_eq!(certs.poll(NOW + 1, Some(NAME)), Some(CertChange::Load));
    *probe.cert.lock().unwrap() = Err("offline".to_string());
    assert_eq!(certs.poll(after, Some(NAME)), Some(CertChange::Clear));
    assert_eq!(
        certs.report().reason.as_deref(),
        Some("the certificate expired")
    );
    assert_eq!(probe.certs_asked(), 2, "and a new one is asked for at once");
}

/// A new MagicDNS name gets a certificate at once, not a day later.
#[test]
fn a_new_magic_dns_name_is_fetched_at_once() {
    let probe = ScriptedProbe::new(&[NAME]);
    let mut certs = lifecycle(&probe, &MemoryFiles::default());
    certs.poll(NOW, Some(NAME));
    certs.poll(NOW + 1, Some(NAME));
    assert_eq!(probe.certs_asked(), 1);

    let renamed = "studio.tail1234.ts.net";
    *probe.status.lock().unwrap() = Some(status(renamed, &[renamed]));
    certs.poll(NOW + 2, Some(renamed));
    assert_eq!(probe.certs_asked(), 2);
    assert_eq!(certs.poll(NOW + 3, Some(renamed)), None, "the same PEMs");
    assert_eq!(certs.current().unwrap().domain.as_deref(), Some(renamed));
}

/// The admin turning HTTPS off withdraws the consent: the certificate is
/// taken down even though it is still valid.
#[test]
fn https_turned_off_for_the_tailnet_takes_the_certificate_down() {
    let probe = ScriptedProbe::new(&[NAME]);
    let mut certs = lifecycle(&probe, &MemoryFiles::default());
    certs.poll(NOW, Some(NAME));
    assert_eq!(certs.poll(NOW + 1, Some(NAME)), Some(CertChange::Load));
    *probe.status.lock().unwrap() = Some(status(NAME, &[]));
    certs.poll(NOW + RENEW_EVERY_MS + 1, Some(NAME));
    assert_eq!(
        certs.poll(NOW + RENEW_EVERY_MS + 2, Some(NAME)),
        Some(CertChange::Clear)
    );
    assert_eq!(certs.report().reason.as_deref(), Some(NOT_ENABLED));
}

/// The files on disk: a private key, in a private directory.
#[test]
fn disk_files_keep_the_key_private() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let paths = crate::worker::paths::WorkerPaths::from_home(home.path());
    let files = DiskCertFiles::new(
        paths.tls_dir.clone(),
        paths.tls_cert.clone(),
        paths.tls_key.clone(),
    );
    assert_eq!(files.load(), None);
    files.save(CERT, KEY).unwrap();
    assert_eq!(files.load(), Some((CERT.to_string(), KEY.to_string())));
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&paths.tls_key), 0o600);
    assert_eq!(mode(&paths.tls_dir), 0o700);
    // Saving again replaces both.
    files.save("c2", "k2").unwrap();
    assert_eq!(files.load(), Some(("c2".to_string(), "k2".to_string())));
    assert_eq!(mode(&paths.tls_key), 0o600);
}
