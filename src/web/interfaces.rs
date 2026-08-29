//! Network interface enumeration for the access overlay's address picker
//! (`specs/WEB_INTERFACE.md` Q1, D5; design `2a` State B).
//!
//! ## Why this exists
//!
//! FlightDeck Web binds `127.0.0.1` by default (D5). That is the right
//! default and the wrong thing to ever put in a QR code: a loopback URL only
//! resolves on the machine that printed it, so a QR encoding it is useless
//! from the phone it is supposedly for. Once the user opts into network
//! access (rebinding `0.0.0.0`), the overlay has to hand out *some* routable
//! address instead — but a host can have several at once (a Wi-Fi adapter, a
//! VM bridge, a VPN tunnel, a second NIC), and only the user knows which
//! network the other device is actually sitting on. So the overlay does not
//! guess: it lists every interface with a one-line, best-effort description
//! and lets the user pick the row to publish (Q1 point 2, "the address is
//! chosen, not guessed").
//!
//! ## Cross-platform parity (flightdeck-cross-platform-parity)
//!
//! This has a hard "behaves identically on macOS, Linux, Windows" requirement,
//! but interface *names* look nothing alike across those three:
//!
//! | Class | macOS | Linux | Windows (friendly name) |
//! | --- | --- | --- | --- |
//! | Wifi | `en0` (see caveat) | `wlan0`, `wlp3s0`, `wlx...` | contains "Wi-Fi" / "Wireless" |
//! | Ethernet | `en1`, `en2`, ... | `eth0`, `enp3s0`, `eno1`, `ens...` | contains "Ethernet" |
//! | VM bridge | `bridge100`, ... | `docker0`, `br-...`, `virbr0` | contains "VMware" / "VirtualBox" / "vEthernet" / "Hyper-V" |
//! | Tunnel | `utun3`, `tailscale0`, `ipsec0`, `ppp0` | `tun0`, `tap0`, `wg0`, `tailscale0` | contains "TAP" / "WireGuard" / "Tailscale" / "VPN" |
//!
//! Rather than branching on `#[cfg(target_os = ...)]`, [`classify`] is a
//! single platform-agnostic name matcher that tries every pattern in the
//! table above unconditionally: only the patterns for the host's actual OS
//! will ever match its actual interfaces, so this needs no knowledge of
//! which OS produced the name, and one set of tests (`tests.rs`) exercises
//! macOS-shaped, Linux-shaped and Windows-shaped fixtures identically.
//!
//! **Reliability is uneven, and that is by design, not an oversight.**
//! Linux's `wl`/`eth`/`en*` prefixes are assigned by the kernel's predictable
//! naming scheme and are reliable. Windows friendly names are human-readable
//! strings and are reliable. **macOS's `en0` is a best-effort guess, not a
//! certainty** — it is the built-in Wi-Fi adapter on the overwhelming
//! majority of Macs (the OS enumerates built-in interfaces first), but some
//! Mac desktops with no wireless card, or docks/adapters that grab `en0`
//! first, break that assumption, and there is no way to tell from the name
//! alone. Any name that does not match a known pattern — including a
//! wifi/ethernet mismatch on macOS — resolves to [`InterfaceClass::Unknown`],
//! which carries **no description**. A wrong "reachable by your phone" is
//! worse than admitting we don't know, so "I don't know" is a first-class
//! variant here rather than an empty string bolted on afterward.
//!
//! ## IPv6 is deliberately out of scope
//!
//! Only IPv4 addresses are enumerated; an interface with only an IPv6
//! address is dropped rather than surfaced with no address. The picker's
//! entire job is to hand a phone a `http://<ip>:<port>/#<code>` URL a human
//! copies, scans, or gets redirected to — IPv6 link-local addresses need a
//! zone/scope id (`fe80::1%en0`) that does not survive being typed or
//! encoded into a QR, and home-network IPv6 routing is inconsistent enough
//! (many consumer routers still don't hand out a stable global IPv6 prefix)
//! that a global IPv6 address is not a dependable substitute. If this
//! becomes a real gap it should be revisited deliberately, not silently.
//!
//! ## Ordering
//!
//! The underlying OS API makes no promise that repeated calls return
//! interfaces in the same order (some platforms enumerate by ifindex, which
//! can change across reconnects/sleep). The overlay is a list the user reads
//! and clicks a row in, so it must not reshuffle between openings:
//! [`finalize`] sorts the final list by `(name, address)`, which is stable
//! and total even when two interfaces happen to share an address.
//!
//! ## Trait seam (flightdeck-architecture-seams)
//!
//! [`InterfaceEnumerator`] is the seam: [`RealInterfaceEnumerator`] wraps
//! `if_addrs::get_if_addrs()`, and `FakeInterfaceEnumerator` supplies
//! OS-shaped fixtures in tests — the latter behind `#[cfg(debug_assertions)]`,
//! so it is not in a release binary at all (§6.5 R26). Both funnel through the
//! same private [`finalize`] pipeline, so tests exercise the *real* filtering,
//! classification and ordering logic, not a reimplementation of it. This
//! trait lives here rather than in `src/contracts/traits.rs` because
//! `web::interfaces` is its only consumer today; move it alongside
//! `GitExecutor`/`FileSystem`/etc. if a second consumer needs it.

#[cfg(test)]
mod tests;

use std::net::Ipv4Addr;

/// One publishable network interface: a name, an IPv4 address, and a
/// best-effort classification. See the module docs for what each
/// classification means and how reliable it is per platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterface {
    pub name: String,
    pub address: Ipv4Addr,
    pub class: InterfaceClass,
}

impl NetworkInterface {
    /// The access overlay's one-line description for this interface (design
    /// `2a` State B: `wifi · reachable by your phone`, `vm bridge`, `your own
    /// tunnel`), or `None` when the name matched no known pattern — the
    /// overlay then shows the raw name and address with no description at
    /// all, rather than a guess.
    pub fn description(&self) -> Option<&'static str> {
        self.class.description()
    }
}

/// A best-effort guess at what kind of interface this is, derived from its
/// name alone. See the module docs for the exact patterns per platform and
/// which are reliable versus best-effort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceClass {
    Wifi,
    Ethernet,
    VmBridge,
    Tunnel,
    /// The name matched none of the known patterns. Carries no description
    /// on purpose: an invented-but-wrong description is worse than none.
    Unknown,
}

impl InterfaceClass {
    fn description(&self) -> Option<&'static str> {
        match self {
            InterfaceClass::Wifi => Some("wifi · reachable by your phone"),
            InterfaceClass::Ethernet => Some("ethernet · reachable by your phone"),
            InterfaceClass::VmBridge => Some("vm bridge"),
            InterfaceClass::Tunnel => Some("your own tunnel"),
            InterfaceClass::Unknown => None,
        }
    }
}

/// Enumerates the host's network interfaces for the access overlay's address
/// picker. See the module docs for the full platform-parity contract.
pub trait InterfaceEnumerator: Send + Sync {
    /// Every non-loopback IPv4 interface on the host, filtered and sorted
    /// per the module docs (loopback excluded, IPv4-only, `(name, address)`
    /// order).
    fn enumerate(&self) -> Vec<NetworkInterface>;
}

/// One raw `(name, optional IPv4 address)` pair as reported by the OS or a
/// test fixture, before loopback exclusion, classification and ordering are
/// applied. `ipv4: None` models an interface with no IPv4 address at all
/// (e.g. IPv6-only) — it is dropped by [`finalize`], never surfaced with a
/// missing address.
#[derive(Debug, Clone)]
struct RawInterface {
    name: String,
    ipv4: Option<Ipv4Addr>,
}

/// The shared pipeline: drop entries with no IPv4 address, drop loopback,
/// classify each remaining name, then sort by `(name, address)` for a
/// stable, documented order. Both [`RealInterfaceEnumerator`] and
/// `FakeInterfaceEnumerator` funnel through this so the same logic is
/// under test regardless of where the raw data came from. (Plain code span,
/// not a doc link: the double is `#[cfg(debug_assertions)]` and is not there
/// to link to in a release-profile doc build.)
fn finalize(raw: Vec<RawInterface>) -> Vec<NetworkInterface> {
    let mut out: Vec<NetworkInterface> = raw
        .into_iter()
        .filter_map(|r| r.ipv4.map(|ip| (r.name, ip)))
        .filter(|(_, ip)| !ip.is_loopback())
        .map(|(name, address)| {
            let class = classify(&name);
            NetworkInterface {
                name,
                address,
                class,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.address.cmp(&b.address)));
    out
}

/// Best-effort classification of an interface name. See the module docs for
/// the per-platform pattern table and the macOS `en0` reliability caveat.
/// Checked in this order deliberately: tunnel names (`tailscale0`, `wg0`,
/// ...) are checked before the broader wifi/ethernet prefixes so a VPN
/// interface never falls through to a weaker match.
fn classify(name: &str) -> InterfaceClass {
    let lower = name.to_ascii_lowercase();

    // --- Tunnel ---------------------------------------------------------
    if lower.contains("tailscale")
        || lower.starts_with("utun")
        || lower.starts_with("tun")
        || lower.starts_with("tap")
        || lower.starts_with("wg")
        || lower.starts_with("ipsec")
        || lower.starts_with("ppp")
        || lower.contains("wireguard")
        || lower.contains("vpn")
    {
        return InterfaceClass::Tunnel;
    }

    // --- VM bridge -------------------------------------------------------
    if lower.starts_with("bridge")
        || lower.starts_with("docker")
        || lower.starts_with("br-")
        || lower.starts_with("virbr")
        || lower.contains("vmware")
        || lower.contains("virtualbox")
        || lower.contains("vethernet")
        || lower.contains("hyper-v")
    {
        return InterfaceClass::VmBridge;
    }

    // --- Wifi -------------------------------------------------------------
    // Linux's predictable-naming scheme reserves the `wl` prefix for
    // wireless (wlan0, wlp3s0, wlx...) — reliable. macOS's `en0` is a
    // best-effort guess (see module docs); it is checked here, not folded
    // into the Ethernet prefix match below.
    if lower.starts_with("wl")
        || lower.contains("wi-fi")
        || lower.contains("wireless")
        || lower == "en0"
    {
        return InterfaceClass::Wifi;
    }

    // --- Ethernet -----------------------------------------------------
    // Linux: eth0 (legacy) and the predictable-naming prefixes enp/eno/ens
    // are reliable. macOS: en1, en2, ... (anything but en0) is a
    // best-effort guess symmetric with the en0-is-wifi guess above.
    if lower.starts_with("eth")
        || lower.starts_with("enp")
        || lower.starts_with("eno")
        || lower.starts_with("ens")
        || lower.contains("ethernet")
        || (lower.starts_with("en") && lower != "en0")
    {
        return InterfaceClass::Ethernet;
    }

    InterfaceClass::Unknown
}

/// [`InterfaceEnumerator`] backed by `if_addrs::get_if_addrs()` (pure Rust
/// over `getifaddrs`/`GetAdaptersAddresses`, no C toolchain — see the
/// dependency comment in `Cargo.toml`).
#[derive(Debug, Clone, Copy, Default)]
pub struct RealInterfaceEnumerator;

impl InterfaceEnumerator for RealInterfaceEnumerator {
    fn enumerate(&self) -> Vec<NetworkInterface> {
        // A enumeration failure (permissions, an exotic sandbox) degrades to
        // an empty list rather than panicking: the overlay can always fall
        // back to "no other address available" and still function.
        let raw = if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .map(|iface| {
                let ipv4 = match iface.addr {
                    if_addrs::IfAddr::V4(v4) => Some(v4.ip),
                    if_addrs::IfAddr::V6(_) => None,
                };
                RawInterface {
                    name: iface.name,
                    ipv4,
                }
            })
            .collect();
        finalize(raw)
    }
}

/// Test double for [`InterfaceEnumerator`], built from OS-shaped fixtures
/// (`(name, ipv4)` pairs). Runs the identical [`finalize`] pipeline the real
/// enumerator uses, so tests exercise the actual classification, loopback
/// exclusion and ordering logic rather than a hand-rolled stand-in for it.
///
/// **Not present in a release build.** The whole double is
/// `#[cfg(debug_assertions)]`, which `cargo build --release` — how every shipped
/// binary is produced (`dist-workspace.toml`, `.github/workflows/release.yml`) —
/// does not compile at all. It is `pub` rather than `#[cfg(test)]` because
/// `tests/web_server.rs` is an external crate and needs it; `debug_assertions`
/// is how this repo already reconciles those two facts for a seam that must not
/// ship (see `CredentialStore::mint_fixed_bootstrap_code`). It lives here rather
/// than in `src/testing/` — the home of the *service-trait* fakes — because
/// [`finalize`] and `RawInterface` are private, and moving the double would mean
/// publishing both of them to serve it (`specs/WEB_INTERFACE.md` §6.5 R26).
#[cfg(debug_assertions)]
#[derive(Debug, Clone, Default)]
pub struct FakeInterfaceEnumerator {
    raw: Vec<RawInterface>,
}

#[cfg(debug_assertions)]
impl FakeInterfaceEnumerator {
    /// An empty fixture (no interfaces at all).
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an interface with an IPv4 address.
    pub fn with_interface(mut self, name: impl Into<String>, ipv4: Ipv4Addr) -> Self {
        self.raw.push(RawInterface {
            name: name.into(),
            ipv4: Some(ipv4),
        });
        self
    }

    /// Add an interface with no IPv4 address at all (e.g. IPv6-only) — it
    /// must be dropped by `enumerate()`, not surfaced with a missing
    /// address.
    pub fn with_interface_no_ipv4(mut self, name: impl Into<String>) -> Self {
        self.raw.push(RawInterface {
            name: name.into(),
            ipv4: None,
        });
        self
    }
}

#[cfg(debug_assertions)]
impl InterfaceEnumerator for FakeInterfaceEnumerator {
    fn enumerate(&self) -> Vec<NetworkInterface> {
        finalize(self.raw.clone())
    }
}
