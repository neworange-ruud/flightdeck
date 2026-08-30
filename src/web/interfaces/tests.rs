//! Tests for `web::interfaces`. Everything is driven through
//! [`FakeInterfaceEnumerator`](super::FakeInterfaceEnumerator) with OS-shaped
//! fixtures, so this suite passes on a CI runner with no wifi and no
//! interesting interfaces. The one exception ([`real_enumerator_does_not_panic_and_excludes_loopback`])
//! exercises the real enumerator and asserts nothing machine-specific.

use super::{
    FakeInterfaceEnumerator, InterfaceClass, InterfaceEnumerator, RealInterfaceEnumerator,
};
use std::net::Ipv4Addr;

fn addr(a: u8, b: u8, c: u8, d: u8) -> Ipv4Addr {
    Ipv4Addr::new(a, b, c, d)
}

#[test]
fn loopback_is_excluded() {
    let fake = FakeInterfaceEnumerator::new()
        .with_interface("lo0", addr(127, 0, 0, 1))
        .with_interface("en0", addr(192, 168, 1, 10));

    let out = fake.enumerate();

    assert!(!out.iter().any(|i| i.address.is_loopback()));
    assert!(!out.iter().any(|i| i.name == "lo0"));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].name, "en0");
}

#[test]
fn macos_shaped_fixture_classifies_each_interface() {
    let fake = FakeInterfaceEnumerator::new()
        .with_interface("lo0", addr(127, 0, 0, 1))
        .with_interface("en0", addr(192, 168, 2, 14))
        .with_interface("bridge100", addr(192, 168, 64, 1))
        .with_interface("utun3", addr(10, 8, 0, 2))
        .with_interface("tailscale0", addr(100, 87, 14, 3));

    let out = fake.enumerate();

    // lo0 must not appear at all.
    assert!(!out.iter().any(|i| i.name == "lo0"));
    assert_eq!(out.len(), 4);

    let by_name = |name: &str| out.iter().find(|i| i.name == name).unwrap();

    assert_eq!(by_name("en0").class, InterfaceClass::Wifi);
    assert_eq!(
        by_name("en0").description(),
        Some("wifi · reachable by your phone")
    );

    assert_eq!(by_name("bridge100").class, InterfaceClass::VmBridge);
    assert_eq!(by_name("bridge100").description(), Some("vm bridge"));

    assert_eq!(by_name("utun3").class, InterfaceClass::Tunnel);
    assert_eq!(by_name("utun3").description(), Some("your own tunnel"));

    assert_eq!(by_name("tailscale0").class, InterfaceClass::Tunnel);
    assert_eq!(by_name("tailscale0").description(), Some("your own tunnel"));
}

#[test]
fn linux_shaped_fixture_classifies_each_interface() {
    let fake = FakeInterfaceEnumerator::new()
        .with_interface("lo", addr(127, 0, 0, 1))
        .with_interface("wlp3s0", addr(192, 168, 1, 22))
        .with_interface("eth0", addr(192, 168, 1, 23))
        .with_interface("docker0", addr(172, 17, 0, 1))
        .with_interface("tun0", addr(10, 6, 0, 5));

    let out = fake.enumerate();

    assert!(!out.iter().any(|i| i.name == "lo"));
    assert_eq!(out.len(), 4);

    let by_name = |name: &str| out.iter().find(|i| i.name == name).unwrap();

    assert_eq!(by_name("wlp3s0").class, InterfaceClass::Wifi);
    assert_eq!(by_name("eth0").class, InterfaceClass::Ethernet);
    assert_eq!(
        by_name("eth0").description(),
        Some("ethernet · reachable by your phone")
    );
    assert_eq!(by_name("docker0").class, InterfaceClass::VmBridge);
    assert_eq!(by_name("tun0").class, InterfaceClass::Tunnel);
}

#[test]
fn windows_shaped_fixture_with_friendly_names() {
    let fake = FakeInterfaceEnumerator::new()
        .with_interface("Loopback Pseudo-Interface 1", addr(127, 0, 0, 1))
        .with_interface("Wi-Fi", addr(192, 168, 1, 40))
        .with_interface("Ethernet", addr(192, 168, 1, 41))
        .with_interface("VMware Network Adapter VMnet8", addr(192, 168, 137, 1))
        .with_interface("Tailscale Tunnel", addr(100, 90, 1, 2));

    let out = fake.enumerate();

    // The pseudo loopback interface's address is loopback, so it is
    // excluded even though the name is nothing like "lo"/"lo0".
    assert!(!out.iter().any(|i| i.name.contains("Loopback")));
    assert_eq!(out.len(), 4);

    let by_name = |name: &str| out.iter().find(|i| i.name == name).unwrap();

    assert_eq!(by_name("Wi-Fi").class, InterfaceClass::Wifi);
    assert_eq!(by_name("Ethernet").class, InterfaceClass::Ethernet);
    assert_eq!(
        by_name("VMware Network Adapter VMnet8").class,
        InterfaceClass::VmBridge
    );
    assert_eq!(by_name("Tailscale Tunnel").class, InterfaceClass::Tunnel);
}

#[test]
fn interface_with_no_ipv4_address_is_dropped() {
    let fake = FakeInterfaceEnumerator::new()
        .with_interface("en0", addr(192, 168, 1, 10))
        .with_interface_no_ipv4("en5"); // e.g. IPv6-only

    let out = fake.enumerate();

    assert_eq!(out.len(), 1);
    assert!(!out.iter().any(|i| i.name == "en5"));
}

#[test]
fn unknown_name_yields_no_invented_description() {
    let fake = FakeInterfaceEnumerator::new().with_interface("gremlin7", addr(10, 0, 0, 9));

    let out = fake.enumerate();

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].class, InterfaceClass::Unknown);
    assert_eq!(out[0].description(), None);
}

#[test]
fn empty_interface_list_yields_empty_output() {
    let fake = FakeInterfaceEnumerator::new();
    assert_eq!(fake.enumerate(), Vec::new());
}

#[test]
fn ordering_is_stable_regardless_of_insertion_order() {
    let fake_a = FakeInterfaceEnumerator::new()
        .with_interface("tailscale0", addr(100, 87, 14, 3))
        .with_interface("bridge100", addr(192, 168, 64, 1))
        .with_interface("en0", addr(192, 168, 2, 14));

    let fake_b = FakeInterfaceEnumerator::new()
        .with_interface("en0", addr(192, 168, 2, 14))
        .with_interface("tailscale0", addr(100, 87, 14, 3))
        .with_interface("bridge100", addr(192, 168, 64, 1));

    let out_a = fake_a.enumerate();
    let out_b = fake_b.enumerate();

    let names_a: Vec<&str> = out_a.iter().map(|i| i.name.as_str()).collect();
    let names_b: Vec<&str> = out_b.iter().map(|i| i.name.as_str()).collect();

    // Sorted by name: bridge100, en0, tailscale0.
    assert_eq!(names_a, vec!["bridge100", "en0", "tailscale0"]);
    assert_eq!(names_a, names_b);

    // Calling again on the same fixture reproduces the same order.
    assert_eq!(out_a, fake_a.enumerate());
}

#[test]
fn duplicate_addresses_on_different_interfaces_both_appear() {
    let fake = FakeInterfaceEnumerator::new()
        .with_interface("br-abc123", addr(172, 20, 0, 1))
        .with_interface("br-def456", addr(172, 20, 0, 1));

    let out = fake.enumerate();

    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|i| i.address == addr(172, 20, 0, 1)));
    // Ordered by name when addresses tie.
    assert_eq!(out[0].name, "br-abc123");
    assert_eq!(out[1].name, "br-def456");
}

#[test]
fn real_enumerator_does_not_panic_and_excludes_loopback() {
    let out = RealInterfaceEnumerator.enumerate();
    // No machine-specific assertions: just that it ran, and that whatever
    // came back never includes a loopback address.
    assert!(!out.iter().any(|i| i.address.is_loopback()));
}
