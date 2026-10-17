//! Where the direct link listens and what it tells the phone to dial
//! (spec §5.10).
//!
//! ## Binding: Tailscale addresses only
//!
//! The direct link mirrors web network mode's posture — opt-in, and never
//! `0.0.0.0` — but narrower: it binds **only** this machine's Tailscale
//! addresses, i.e. interface addresses inside Tailscale's CGNAT range
//! `100.64.0.0/10` or its ULA range `fd7a:115c:a1e0::/48`. Tailscale's
//! WireGuard tunnel is what provides reachability (an iPhone on cellular) and
//! transport encryption; a phone on the same Wi-Fi without Tailscale cannot
//! reach the socket at all, which is the point. Selection is by **address**,
//! not interface name, because the name differs per OS (`utun4` on macOS,
//! `tailscale0` on Linux, "Tailscale" on Windows) while the ranges are
//! Tailscale's own, identical everywhere — so [`tailscale_addresses`] is one
//! platform-agnostic filter with no `cfg`.
//!
//! `src/web/interfaces.rs` is not reused: it is deliberately IPv4-only (its
//! module docs explain why a QR code cannot carry a link-local IPv6 address),
//! classifies by interface *name*, and drops nothing by range — the direct link
//! needs the opposite on all three counts. Both use the same `if-addrs` crate.
//!
//! ### Which addresses are Tailscale's: ask Tailscale first
//!
//! `100.64.0.0/10` is the shared address space of RFC 6598 — Tailscale uses it,
//! but so does carrier-grade NAT, so a laptop tethered to a phone or on some
//! ISPs holds a `100.x` address that is *not* Tailscale, and binding it would
//! expose the direct link to that carrier network. So the addresses
//! `tailscale status --json` reports for this node (`Self.TailscaleIPs`) win
//! whenever the CLI answers: only interface addresses Tailscale itself claims
//! are bound — none at all when it reports none (logged out, stopped). The
//! range check ([`tailscale_addresses`]) is only the fallback for when the CLI
//! cannot be asked (not installed or not on `PATH`, a sandbox that forbids
//! spawning). [`select_bind_addresses`] makes that choice and reports which
//! source it used, which the listener logs.
//!
//! ## Advertising: config, then MagicDNS, then the 100.x address
//!
//! [`advertised_host`] picks, in order: the `[remote] direct_host` override;
//! this machine's Tailscale MagicDNS name (`Self.DNSName` from
//! `tailscale status --json`, trailing dot stripped); else the first bound
//! Tailscale IPv4 address (then IPv6). MagicDNS is preferred because it survives
//! a Tailscale IP change; the `100.x` address is stable per node anyway.
//!
//! ## Seams (flightdeck-architecture-seams)
//!
//! Both side effects sit behind traits so tests never touch the network stack
//! or spawn a process: [`AddressSource`] (real: `if_addrs`) and
//! [`TailscaleProbe`] (real: the `tailscale` CLI). The CLI is spawned by argv,
//! never through a shell, bounded by a timeout, and **degrades to `None`** on
//! every failure — not installed (the common case on Linux/Windows), not logged
//! in, a sandbox that forbids spawning — so the direct link still advertises the
//! raw address (flightdeck-cross-platform-parity).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long the `tailscale status --json` probe may run.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Whether `ip` is a Tailscale address: `100.64.0.0/10` (IPv4 CGNAT) or
/// `fd7a:115c:a1e0::/48` (Tailscale's IPv6 ULA prefix).
pub fn is_tailscale_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            // 100.64.0.0/10 → first octet 100, top two bits of the second 01.
            o[0] == 100 && (o[1] & 0b1100_0000) == 0b0100_0000
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// The Tailscale addresses among `all`, de-duplicated, IPv4 first, each family
/// in ascending order (stable across calls whatever order the OS enumerates in).
pub fn tailscale_addresses(all: &[IpAddr]) -> Vec<IpAddr> {
    sorted_unique(
        all.iter()
            .copied()
            .filter(|ip| is_tailscale_address(*ip))
            .collect(),
    )
}

/// The host to advertise in `pairing_offer.direct_endpoint`, or `None` when
/// nothing is bound (then no direct endpoint is offered and the pairing is
/// relay mode). See the module docs for the precedence.
pub fn advertised_host(
    configured: &str,
    magic_dns: Option<&str>,
    bound: &[IpAddr],
) -> Option<String> {
    if bound.is_empty() {
        return None;
    }
    let configured = configured.trim();
    if !configured.is_empty() {
        return Some(configured.to_string());
    }
    if let Some(name) = magic_dns.map(str::trim).filter(|n| !n.is_empty()) {
        return Some(name.to_string());
    }
    bound
        .iter()
        .find(|ip| ip.is_ipv4())
        .or_else(|| bound.first())
        .map(IpAddr::to_string)
}

/// What `tailscale status --json` says about this node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TailscaleStatus {
    /// `Self.DNSName` without the trailing root dot, if MagicDNS is on.
    pub magic_dns: Option<String>,
    /// `Self.TailscaleIPs`: the addresses Tailscale assigned this node. Empty
    /// when it reports none (logged out, stopped, no `Self`).
    pub ips: Vec<IpAddr>,
}

/// Parse `tailscale status --json` output. `None` only for output that is not
/// JSON at all; a CLI that answered (even "needs login") yields a status,
/// possibly with no addresses — which is itself authoritative.
pub fn status_from_json(json: &str) -> Option<TailscaleStatus> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let me = value.get("Self");
    let magic_dns = me
        .and_then(|m| m.get("DNSName"))
        .and_then(|n| n.as_str())
        .map(|n| n.trim().trim_end_matches('.').to_string())
        .filter(|n| !n.is_empty());
    let ips = me
        .and_then(|m| m.get("TailscaleIPs"))
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|ip| ip.as_str()?.parse::<IpAddr>().ok())
                .collect()
        })
        .unwrap_or_default();
    Some(TailscaleStatus { magic_dns, ips })
}

/// Pull `Self.DNSName` out of `tailscale status --json` output, without the
/// trailing root dot. `None` for anything unparsable or empty.
pub fn magic_dns_from_status_json(json: &str) -> Option<String> {
    status_from_json(json)?.magic_dns
}

/// Where the bind addresses came from (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressOrigin {
    /// The addresses `tailscale status --json` reports for this node.
    TailscaleStatus,
    /// The Tailscale address ranges, because the CLI could not be asked. A
    /// carrier-grade-NAT `100.x` address is indistinguishable here.
    RangeFallback,
}

impl AddressOrigin {
    /// A short label for log lines.
    pub fn label(self) -> &'static str {
        match self {
            AddressOrigin::TailscaleStatus => "tailscale status",
            AddressOrigin::RangeFallback => "address-range fallback (tailscale CLI unavailable)",
        }
    }
}

/// The addresses to bind among this host's interface addresses `all`: those
/// Tailscale `reported` for this node when the CLI answered (even an empty
/// list — then nothing is bound), else the range filter. De-duplicated, IPv4
/// first, each family ascending.
pub fn select_bind_addresses(
    all: &[IpAddr],
    reported: Option<&[IpAddr]>,
) -> (Vec<IpAddr>, AddressOrigin) {
    match reported {
        Some(reported) => {
            let claimed: Vec<IpAddr> = all
                .iter()
                .copied()
                .filter(|ip| reported.contains(ip))
                .collect();
            (sorted_unique(claimed), AddressOrigin::TailscaleStatus)
        }
        None => (tailscale_addresses(all), AddressOrigin::RangeFallback),
    }
}

/// `ips` de-duplicated, IPv4 first, each family ascending.
fn sorted_unique(ips: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut v4: Vec<Ipv4Addr> = Vec::new();
    let mut v6: Vec<Ipv6Addr> = Vec::new();
    for ip in ips {
        match ip {
            IpAddr::V4(a) => v4.push(a),
            IpAddr::V6(a) => v6.push(a),
        }
    }
    v4.sort_unstable();
    v4.dedup();
    v6.sort_unstable();
    v6.dedup();
    v4.into_iter()
        .map(IpAddr::V4)
        .chain(v6.into_iter().map(IpAddr::V6))
        .collect()
}

/// Enumerates this host's interface addresses (every family, loopback
/// included — [`tailscale_addresses`] does the filtering).
pub trait AddressSource: Send + Sync {
    /// All interface addresses currently assigned.
    fn addresses(&self) -> Vec<IpAddr>;
}

/// [`AddressSource`] backed by `if_addrs` (getifaddrs / GetAdaptersAddresses).
/// An enumeration failure degrades to "no addresses" (so: nothing bound).
#[derive(Debug, Clone, Copy, Default)]
pub struct RealAddressSource;

impl AddressSource for RealAddressSource {
    fn addresses(&self) -> Vec<IpAddr> {
        if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .map(|iface| match iface.addr {
                if_addrs::IfAddr::V4(v4) => IpAddr::V4(v4.ip),
                if_addrs::IfAddr::V6(v6) => IpAddr::V6(v6.ip),
            })
            .collect()
    }
}

/// A fixed address list, for tests.
#[derive(Debug, Clone, Default)]
pub struct FixedAddressSource(pub Vec<IpAddr>);

impl AddressSource for FixedAddressSource {
    fn addresses(&self) -> Vec<IpAddr> {
        self.0.clone()
    }
}

/// Asks Tailscale about this machine (`tailscale status --json`).
pub trait TailscaleProbe: Send + Sync {
    /// This node's status, or `None` when the CLI cannot be asked at all (not
    /// installed, not on `PATH`, spawning forbidden, timed out). Blocking: call
    /// it off the async workers.
    fn status(&self) -> Option<TailscaleStatus>;

    /// The MagicDNS name, or `None` if Tailscale is absent, logged out, or
    /// MagicDNS is off. Blocking, like [`Self::status`].
    fn magic_dns_name(&self) -> Option<String> {
        self.status().and_then(|s| s.magic_dns)
    }
}

/// [`TailscaleProbe`] that runs `tailscale status --json` (argv, no shell),
/// trying `tailscale` on `PATH` and then the macOS app bundle's CLI, each
/// bounded by [`PROBE_TIMEOUT`]. Every failure is `None`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CliTailscaleProbe;

/// Where to look for the CLI. The App Store / standalone macOS app ships it
/// inside the bundle and does not always put it on `PATH`; elsewhere the
/// package manager does. Trying a path that does not exist is a cheap `None`.
const TAILSCALE_CANDIDATES: &[&str] = &[
    "tailscale",
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
];

impl TailscaleProbe for CliTailscaleProbe {
    fn status(&self) -> Option<TailscaleStatus> {
        TAILSCALE_CANDIDATES
            .iter()
            .filter(|c| !c.starts_with('/') || std::path::Path::new(c).exists())
            .find_map(|program| {
                run_bounded(program, &["status", "--json"], PROBE_TIMEOUT)
                    .and_then(|out| status_from_json(&out))
            })
    }
}

/// A probe with a fixed answer, for tests.
#[derive(Debug, Clone, Default)]
pub struct FixedTailscaleProbe(pub Option<TailscaleStatus>);

impl TailscaleProbe for FixedTailscaleProbe {
    fn status(&self) -> Option<TailscaleStatus> {
        self.0.clone()
    }
}

/// Run `program args…` and return its stdout if it exits successfully within
/// `limit`; kill it and return `None` otherwise. Output is read on a helper
/// thread so a chatty child can never fill the pipe and stall the wait.
fn run_bounded(program: &str, args: &[&str], limit: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut buf);
        buf
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = reader.join().ok()?;
    status.filter(|s| s.success()).map(|_| out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_the_tailscale_ranges_qualify() {
        // 100.64.0.0/10 spans 100.64.0.0 – 100.127.255.255.
        assert!(is_tailscale_address(ip("100.64.0.0")));
        assert!(is_tailscale_address(ip("100.101.102.103")));
        assert!(is_tailscale_address(ip("100.127.255.255")));
        assert!(!is_tailscale_address(ip("100.63.255.255")));
        assert!(!is_tailscale_address(ip("100.128.0.0")));
        assert!(!is_tailscale_address(ip("192.168.1.20")));
        assert!(!is_tailscale_address(ip("10.0.0.1")));
        assert!(!is_tailscale_address(ip("127.0.0.1")));
        assert!(!is_tailscale_address(ip("0.0.0.0")));
        // fd7a:115c:a1e0::/48.
        assert!(is_tailscale_address(ip("fd7a:115c:a1e0::1")));
        assert!(is_tailscale_address(ip(
            "fd7a:115c:a1e0:ab12:4843:cd96:6258:b240"
        )));
        assert!(!is_tailscale_address(ip("fd7a:115c:a1e1::1")));
        assert!(!is_tailscale_address(ip("fe80::1")));
        assert!(!is_tailscale_address(ip("::1")));
        assert!(!is_tailscale_address(ip("::")));
    }

    #[test]
    fn selection_filters_dedups_and_orders_ipv4_first() {
        let all = [
            ip("fd7a:115c:a1e0::9"),
            ip("192.168.1.20"),
            ip("100.100.1.2"),
            ip("127.0.0.1"),
            ip("::1"),
            ip("100.64.0.7"),
            ip("100.100.1.2"),
            ip("fe80::1"),
        ];
        assert_eq!(
            tailscale_addresses(&all),
            vec![ip("100.64.0.7"), ip("100.100.1.2"), ip("fd7a:115c:a1e0::9")]
        );
        assert!(tailscale_addresses(&[ip("192.168.1.2"), ip("::1")]).is_empty());
    }

    #[test]
    fn advertised_host_precedence() {
        let bound = [ip("fd7a:115c:a1e0::9"), ip("100.100.1.2")];
        assert_eq!(
            advertised_host(" mac.example ", Some("mbp.tail1234.ts.net"), &bound).as_deref(),
            Some("mac.example"),
            "the config override wins"
        );
        assert_eq!(
            advertised_host("", Some("mbp.tail1234.ts.net"), &bound).as_deref(),
            Some("mbp.tail1234.ts.net"),
            "then MagicDNS"
        );
        assert_eq!(
            advertised_host("", None, &bound).as_deref(),
            Some("100.100.1.2"),
            "then the 100.x address, preferred over IPv6"
        );
        assert_eq!(
            advertised_host("  ", Some(" "), &[ip("fd7a:115c:a1e0::9")]).as_deref(),
            Some("fd7a:115c:a1e0::9"),
            "blank values fall through; IPv6 when that is all there is"
        );
        assert_eq!(
            advertised_host("mac.example", Some("x"), &[]),
            None,
            "nothing bound means nothing to advertise"
        );
    }

    #[test]
    fn magic_dns_is_read_from_status_json() {
        let json = r#"{"Version":"1.70.0","Self":{"DNSName":"ruuds-mbp.tail1234.ts.net.",
                       "TailscaleIPs":["100.100.1.2"]}}"#;
        assert_eq!(
            magic_dns_from_status_json(json).as_deref(),
            Some("ruuds-mbp.tail1234.ts.net")
        );
        assert_eq!(
            magic_dns_from_status_json(r#"{"Self":{"DNSName":""}}"#),
            None
        );
        assert_eq!(
            magic_dns_from_status_json(r#"{"BackendState":"NeedsLogin"}"#),
            None
        );
        assert_eq!(magic_dns_from_status_json("not json"), None);
    }

    #[test]
    fn status_reports_the_nodes_tailscale_ips() {
        let json = r#"{"Self":{"DNSName":"mbp.tail1234.ts.net.",
                       "TailscaleIPs":["100.100.1.2","fd7a:115c:a1e0::9","bogus"]}}"#;
        assert_eq!(
            status_from_json(json),
            Some(TailscaleStatus {
                magic_dns: Some("mbp.tail1234.ts.net".into()),
                ips: vec![ip("100.100.1.2"), ip("fd7a:115c:a1e0::9")],
            })
        );
        // The CLI answered "needs login": a status with no addresses, which is
        // authoritative (bind nothing), not "unknown".
        assert_eq!(
            status_from_json(r#"{"BackendState":"NeedsLogin"}"#),
            Some(TailscaleStatus::default())
        );
        assert_eq!(status_from_json("not json"), None);
    }

    /// A carrier-grade-NAT `100.x` address sits in Tailscale's range too. When
    /// the CLI answers, only the addresses Tailscale claims for this node are
    /// bound; the range check is the fallback for when it cannot be asked.
    #[test]
    fn the_cli_reported_addresses_win_over_the_range_check() {
        let all = [
            ip("100.71.4.9"), // carrier CGNAT on a tethered link
            ip("100.100.1.2"),
            ip("fd7a:115c:a1e0::9"),
            ip("192.168.1.20"),
        ];
        let reported = [ip("100.100.1.2"), ip("fd7a:115c:a1e0::9"), ip("100.1.1.1")];
        assert_eq!(
            select_bind_addresses(&all, Some(&reported)),
            (
                vec![ip("100.100.1.2"), ip("fd7a:115c:a1e0::9")],
                AddressOrigin::TailscaleStatus
            ),
            "the CGNAT address is not bound; a reported but unassigned one neither"
        );
        assert_eq!(
            select_bind_addresses(&all, Some(&[])),
            (vec![], AddressOrigin::TailscaleStatus),
            "Tailscale says it has no address (logged out): bind nothing"
        );
        assert_eq!(
            select_bind_addresses(&all, None),
            (
                vec![ip("100.71.4.9"), ip("100.100.1.2"), ip("fd7a:115c:a1e0::9")],
                AddressOrigin::RangeFallback
            ),
            "no CLI: the range check, CGNAT and all"
        );
    }

    /// A missing CLI is the normal case off macOS; it must degrade to `None`,
    /// never panic or hang.
    #[test]
    fn a_missing_program_degrades_to_none() {
        assert_eq!(
            run_bounded(
                "flightdeck-definitely-not-a-real-program",
                &["status"],
                Duration::from_secs(1)
            ),
            None
        );
    }

    #[test]
    fn fixed_seams_return_their_fixtures() {
        let src = FixedAddressSource(vec![ip("100.64.0.1")]);
        assert_eq!(src.addresses(), vec![ip("100.64.0.1")]);
        let probe = FixedTailscaleProbe(Some(TailscaleStatus {
            magic_dns: Some("m.ts.net".into()),
            ips: vec![ip("100.64.0.1")],
        }));
        assert_eq!(probe.magic_dns_name().as_deref(), Some("m.ts.net"));
        assert_eq!(FixedTailscaleProbe(None).magic_dns_name(), None);
    }
}
