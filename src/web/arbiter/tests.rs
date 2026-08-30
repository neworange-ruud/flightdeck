use std::sync::{Arc, Mutex};

use super::*;

fn viewer(id: &str) -> Writer {
    Writer::Viewer(ViewerId::new(id.to_string()))
}

#[test]
fn a_free_lock_is_taken_by_whoever_types_first() {
    let mut lock = InputArbiter::new();
    assert_eq!(lock.holder(), None);
    assert_eq!(
        lock.claim(&viewer("v0"), "192.168.2.20", 1_000),
        Claim::Granted
    );
    assert_eq!(lock.holder(), Some(&viewer("v0")));
    assert_eq!(lock.holder_label(), Some("192.168.2.20"));
}

#[test]
fn a_live_burst_refuses_everyone_else_by_name() {
    let mut lock = InputArbiter::new();
    lock.claim(&viewer("v0"), "192.168.2.20", 1_000);
    // One keystroke later, well inside the idle window.
    assert_eq!(
        lock.claim(&Writer::Desktop, "desktop", 1_050),
        Claim::Refused {
            by: viewer("v0"),
            label: "192.168.2.20".to_string(),
        },
        "the refusal must name the holder — 'it did not work' is indistinguishable \
         from a broken host"
    );
    // And the refusal changed nothing: the holder still holds it.
    assert_eq!(lock.holder(), Some(&viewer("v0")));
}

#[test]
fn the_desktop_has_no_precedence_over_a_browser() {
    // The whole point of the model: a surface that can always cut in is the
    // corruption this removes. The rule is symmetric in both directions, and
    // this asserts both so a future "the desktop is special" cannot pass.
    let mut browser_first = InputArbiter::new();
    browser_first.claim(&viewer("v0"), "192.168.2.20", 1_000);
    assert!(matches!(
        browser_first.claim(&Writer::Desktop, "desktop", 1_100),
        Claim::Refused { .. }
    ));

    let mut desktop_first = InputArbiter::new();
    desktop_first.claim(&Writer::Desktop, "desktop", 1_000);
    assert!(matches!(
        desktop_first.claim(&viewer("v0"), "192.168.2.20", 1_100),
        Claim::Refused { .. }
    ));
}

#[test]
fn the_holder_keeps_it_by_typing_and_loses_it_by_stopping() {
    let mut lock = InputArbiter::with_idle_ms(400);
    lock.claim(&Writer::Desktop, "desktop", 1_000);
    // A burst: each keystroke refreshes the hold, so the window never opens
    // even though 1_900 is 900ms after the first key.
    for now in [1_200, 1_500, 1_900] {
        assert_eq!(lock.claim(&Writer::Desktop, "desktop", now), Claim::Granted);
        assert!(matches!(
            lock.claim(&viewer("v0"), "192.168.2.20", now + 10),
            Claim::Refused { .. }
        ));
    }
    // Exactly the idle window after the last keystroke, the lock is takeable.
    assert!(matches!(
        lock.claim(&viewer("v0"), "192.168.2.20", 1_900 + 399),
        Claim::Refused { .. }
    ));
    assert_eq!(
        lock.claim(&viewer("v0"), "192.168.2.20", 1_900 + 400),
        Claim::Granted
    );
    assert_eq!(lock.holder(), Some(&viewer("v0")));
}

#[test]
fn preemption_cuts_into_a_live_burst_and_nothing_else_does() {
    let mut lock = InputArbiter::new();
    lock.claim(&viewer("v0"), "192.168.2.20", 1_000);
    assert!(matches!(
        lock.claim(&Writer::Desktop, "desktop", 1_010),
        Claim::Refused { .. }
    ));
    lock.preempt(&Writer::Desktop, "desktop", 1_020);
    assert_eq!(lock.holder(), Some(&Writer::Desktop));
    // And now the browser is the one being refused, on the same rule.
    assert!(matches!(
        lock.claim(&viewer("v0"), "192.168.2.20", 1_030),
        Claim::Refused { .. }
    ));
}

#[test]
fn expiry_publishes_free_rather_than_the_last_person_to_type() {
    let mut lock = InputArbiter::with_idle_ms(400);
    lock.claim(&viewer("v0"), "192.168.2.20", 1_000);
    assert!(!lock.expire(1_300), "still mid-burst: nothing to announce");
    assert_eq!(lock.holder(), Some(&viewer("v0")));
    assert!(
        lock.expire(1_400),
        "gone quiet: the chip must stop naming them"
    );
    assert_eq!(lock.holder(), None);
    assert!(
        !lock.expire(9_999),
        "already free: nothing changed, nothing announced"
    );
}

#[test]
fn a_departing_writer_gives_the_lock_back_immediately() {
    // A closed socket must not hold the terminal for another 400ms.
    let mut lock = InputArbiter::new();
    lock.claim(&viewer("v0"), "192.168.2.20", 1_000);
    lock.release(&viewer("v1"));
    assert_eq!(
        lock.holder(),
        Some(&viewer("v0")),
        "somebody else leaving is not our release"
    );
    lock.release(&viewer("v0"));
    assert_eq!(lock.holder(), None);
}

/// Two writers, two threads, one lock — the property the whole model exists for,
/// asserted without a socket in the way.
///
/// Real threads and a real clock, with the idle window shrunk to 50 ms so the
/// test finishes: each burst of five keystrokes takes microseconds, three orders
/// of magnitude inside the window, so a burst physically cannot idle out inside
/// itself. Each thread retries a burst it was refused until it lands, so both
/// genuinely reach the terminal rather than one starving.
///
/// The assertion that matters is the one inside the loop: **no burst is ever
/// half-granted**. A burst that starts is never interrupted, so the bytes a PTY
/// would have seen are whole bursts in claim order and never two writers' bytes
/// spliced together.
#[test]
fn two_threads_typing_at_once_never_split_a_burst() {
    use std::time::Instant;

    const IDLE_MS: i64 = 50;
    const BURSTS: usize = 20;
    const KEYS: usize = 5;

    let start = Instant::now();
    let lock = Arc::new(Mutex::new(InputArbiter::with_idle_ms(IDLE_MS)));
    let mut handles = Vec::new();

    for (name, who) in [("desktop", Writer::Desktop), ("browser", viewer("v0"))] {
        let lock = Arc::clone(&lock);
        handles.push(std::thread::spawn(move || {
            let now = || start.elapsed().as_millis() as i64;
            for burst in 0..BURSTS {
                let deadline = Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    let mut granted = 0usize;
                    for _ in 0..KEYS {
                        match lock.lock().expect("not poisoned").claim(&who, name, now()) {
                            Claim::Granted => granted += 1,
                            Claim::Refused { .. } => break,
                        }
                    }
                    if granted == KEYS {
                        break;
                    }
                    assert_eq!(
                        granted, 0,
                        "burst split after {granted} of {KEYS} keystrokes — a PTY \
                         would have seen half a word"
                    );
                    // Refused outright, which is the model working. Wait for the
                    // holder to go quiet and try the whole burst again.
                    std::thread::sleep(std::time::Duration::from_millis(IDLE_MS as u64 + 10));
                    assert!(
                        Instant::now() < deadline,
                        "{name} never got a turn for burst {burst}"
                    );
                }
                // Our own pause between bursts, which is what lets the other
                // writer in.
                std::thread::sleep(std::time::Duration::from_millis(IDLE_MS as u64 + 10));
            }
        }));
    }

    for handle in handles {
        handle.join().expect("neither writer saw a split burst");
    }
}
