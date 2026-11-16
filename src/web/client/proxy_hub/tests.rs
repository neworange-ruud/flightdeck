use std::io::{Read, Write};
use std::net::{TcpListener as StdListener, TcpStream as StdStream};
use std::thread;

use base64::Engine;

use super::*;

/// `Proxy-Authorization` for `session`.
fn login(session: &HubSession) -> String {
    let (user, pass) = session.credentials();
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
    )
}

/// A connection to the hub with a five-second read timeout.
fn dial(hub: &ProxyHub) -> StdStream {
    let stream = StdStream::connect(("127.0.0.1", hub.port())).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
}

/// Send `request` and read until the hub closes.
fn ask(hub: &ProxyHub, request: &str) -> String {
    let mut stream = dial(hub);
    stream.write_all(request.as_bytes()).unwrap();
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);
    answer
}

/// A server on this machine that echoes what it is sent.
fn echo_server() -> u16 {
    let listener = StdListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut buf = [0u8; 1024];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 || stream.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

/// A server on this machine that answers each request with the head it
/// received, as the body.
fn mirror_server() -> u16 {
    let listener = StdListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read_exact(&mut byte).is_ok() {
                    head.push(byte[0]);
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    head.len()
                );
                let _ = stream.write_all(&head);
            });
        }
    });
    port
}

fn free_port() -> u16 {
    let listener = StdListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[test]
fn targets_go_to_the_worker_as_host_and_port_with_ipv6_bracketed() {
    assert_eq!(
        normalize_target("localhost:3000", None).as_deref(),
        Some("localhost:3000")
    );
    assert_eq!(
        normalize_target("[::1]:3000", None).as_deref(),
        Some("[::1]:3000")
    );
    // A SOCKS-style name: the last part is the port.
    assert_eq!(
        normalize_target("::1:3000", None).as_deref(),
        Some("[::1]:3000")
    );
    assert_eq!(
        normalize_target("example.com", Some(80)).as_deref(),
        Some("example.com:80")
    );
    assert_eq!(
        normalize_target("[::1]", Some(80)).as_deref(),
        Some("[::1]:80")
    );
    // CONNECT always names its port.
    assert_eq!(normalize_target("example.com", None), None);
    assert_eq!(normalize_target("example.com:0", None), None);
    assert_eq!(normalize_target("example.com:http", None), None);
    assert_eq!(normalize_target("user@example.com:80", None), None);
    assert_eq!(normalize_target(":80", None), None);
}

#[test]
fn only_plain_http_urls_are_taken_in_absolute_form() {
    let split = absolute_http;
    assert_eq!(
        split("http://localhost:3000/a/b?c=1"),
        Some(("localhost:3000".into(), "/a/b?c=1".into()))
    );
    assert_eq!(
        split("http://example.com"),
        Some(("example.com".into(), "/".into()))
    );
    assert_eq!(
        split("HTTP://example.com?x"),
        Some(("example.com".into(), "/?x".into()))
    );
    assert_eq!(
        split("http://[::1]:5173/#frag"),
        Some(("[::1]:5173".into(), "/".into()))
    );
    assert_eq!(split("https://example.com/"), None);
    assert_eq!(split("http://user:pw@example.com/"), None);
    assert_eq!(split("/relative"), None);
}

#[test]
fn a_forwarded_request_loses_the_proxys_headers_and_closes() {
    let request = Request::parse(
        "GET http://h/x HTTP/1.1\r\nHost: h\r\nProxy-Authorization: Basic abc\r\n\
         Proxy-Connection: keep-alive\r\nConnection: keep-alive\r\nKeep-Alive: 5\r\n\
         Accept: */*\r\n\r\n",
    )
    .unwrap();
    let forwarded = request.origin_form("/x");
    assert!(forwarded.starts_with("GET /x HTTP/1.1\r\n"), "{forwarded}");
    assert!(forwarded.contains("Host: h\r\n"));
    assert!(forwarded.contains("Accept: */*\r\n"));
    assert!(forwarded.ends_with("Connection: close\r\n\r\n"));
    let lower = forwarded.to_ascii_lowercase();
    assert!(!lower.contains("proxy-"), "{forwarded}");
    assert!(!lower.contains("keep-alive"), "{forwarded}");
    assert_eq!(lower.matches("connection:").count(), 1);
}

#[test]
fn no_login_a_wrong_one_or_a_closed_sessions_is_refused_with_a_challenge() {
    let hub = ProxyHub::start().unwrap();
    let session = hub.direct_session();
    let port = echo_server();

    let none = ask(&hub, &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n"));
    assert!(none.starts_with("HTTP/1.1 407 "), "{none}");
    assert!(none.contains("Proxy-Authenticate: Basic realm=\"FlightDeck\""));

    let (user, _) = session.credentials();
    let wrong = base64::engine::general_purpose::STANDARD.encode(format!("{user}:nope"));
    let answer = ask(
        &hub,
        &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nProxy-Authorization: Basic {wrong}\r\n\r\n"),
    );
    assert!(answer.starts_with("HTTP/1.1 407 "), "{answer}");

    let closed = login(&session);
    drop(session);
    let answer = ask(
        &hub,
        &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nProxy-Authorization: {closed}\r\n\r\n"),
    );
    assert!(answer.starts_with("HTTP/1.1 407 "), "{answer}");
}

#[test]
fn sessions_have_logins_of_their_own() {
    let hub = ProxyHub::start().unwrap();
    let a = hub.direct_session();
    let b = hub.direct_session();
    assert_ne!(a.credentials().0, b.credentials().0);
    assert_ne!(a.credentials().1, b.credentials().1);
    assert!(
        !a.credentials().0.contains(':'),
        "a Basic username has no ':'"
    );
}

#[test]
fn connect_carries_bytes_both_ways_for_a_local_session() {
    let hub = ProxyHub::start().unwrap();
    let session = hub.direct_session();
    let port = echo_server();
    let mut stream = dial(&hub);
    write!(
        stream,
        "CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nProxy-Authorization: {}\r\n\r\n",
        login(&session)
    )
    .unwrap();
    let established = b"HTTP/1.1 200 Connection established\r\n\r\n";
    let mut answer = vec![0u8; established.len()];
    stream.read_exact(&mut answer).unwrap();
    assert_eq!(answer, established);
    stream.write_all(b"ping").unwrap();
    let mut echo = [0u8; 4];
    stream.read_exact(&mut echo).unwrap();
    assert_eq!(&echo, b"ping");
}

#[test]
fn plain_http_is_sent_in_origin_form_and_its_answer_comes_back() {
    let hub = ProxyHub::start().unwrap();
    let session = hub.direct_session();
    let port = mirror_server();
    let answer = ask(
        &hub,
        &format!(
            "GET http://127.0.0.1:{port}/page?x=1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
             Proxy-Authorization: {}\r\nProxy-Connection: keep-alive\r\n\r\n",
            login(&session)
        ),
    );
    assert!(answer.starts_with("HTTP/1.1 200 OK"), "{answer}");
    // The body is the head the server received.
    assert!(answer.contains("GET /page?x=1 HTTP/1.1\r\n"), "{answer}");
    assert!(answer.contains("Connection: close\r\n"), "{answer}");
    assert!(!answer.contains("Proxy-Authorization"), "{answer}");
    assert!(!answer.contains("Proxy-Connection"), "{answer}");
}

#[test]
fn an_unreachable_target_is_a_502_and_an_event_for_the_error_page() {
    let hub = ProxyHub::start().unwrap();
    let session = hub.direct_session();
    let dead = free_port();
    let answer = ask(
        &hub,
        &format!(
            "CONNECT 127.0.0.1:{dead} HTTP/1.1\r\nProxy-Authorization: {}\r\n\r\n",
            login(&session)
        ),
    );
    assert!(answer.starts_with("HTTP/1.1 502 "), "{answer}");
    assert!(answer.contains("on this computer"), "{answer}");
    let events = session.take_events();
    assert!(
        events.iter().any(|e| matches!(e,
            RelayEvent::Refused { target, reason, .. }
                if target == &format!("127.0.0.1:{dead}") && reason == "refused")),
        "{events:?}"
    );
    assert!(session.take_events().is_empty(), "events are taken once");
}

#[test]
fn not_a_proxy_request_is_a_400() {
    let hub = ProxyHub::start().unwrap();
    let session = hub.direct_session();
    let answer = ask(
        &hub,
        &format!(
            "GET /relative HTTP/1.1\r\nProxy-Authorization: {}\r\n\r\n",
            login(&session)
        ),
    );
    assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
    let answer = ask(&hub, "nonsense\r\n\r\n");
    assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
}
