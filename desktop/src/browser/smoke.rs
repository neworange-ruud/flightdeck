//! `flightdeck-desktop --spike-browser`: the real [`BrowserView`] in a real
//! window, against a stand-in worker, printing what it loaded, then quitting.
//! A debugging aid like the terminal spike, not a test: GPUI's test platform
//! has no native window to hang a web view on.
//!
//! The stand-in answers `GET /tunnel` the way a worker does (no cookie or
//! key check) and connects **every** target to one page server, which names
//! the `Host` it was asked for in its title. So:
//!
//! - a name only the far side knows loading means the web view's proxy, the
//!   relay's SOCKS door and its password all work;
//! - `localhost:<P>` loading means the relay forwarded `P` (B22, B24);
//! - `file:///` must come back as our error page, never a file.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use flightdeck::web::client::exchange::AccessToken;
use flightdeck::web::client::http::HttpAccess;
use gpui::{px, size, App, AppContext, Bounds, WindowBounds, WindowOptions};
use gpui_component::Root;

use super::view::BrowserView;

/// Run it. Returns the exit code.
pub fn run() -> i32 {
    let page = page_server();
    let worker = stand_in_worker(page);
    let free = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let loopback = free.local_addr().unwrap().port();
    drop(free);
    println!("page server 127.0.0.1:{page}, stand-in worker 127.0.0.1:{worker}");

    gpui_platform::application()
        .with_assets(crate::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            let bounds = Bounds::centered(None, size(px(1000.), px(700.)), cx);
            let mut view = None;
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    let browser = cx.new(|cx| {
                        BrowserView::new(
                            HttpAccess {
                                address: format!("127.0.0.1:{worker}"),
                                token: AccessToken::new("smoke".to_string()),
                                user_agent: "FlightDeck Desktop/smoke".to_string(),
                            },
                            "smoke-key".to_string(),
                            "the stand-in worker".to_string(),
                            super::profile_id("smoke", "smoke"),
                            window,
                            cx,
                        )
                    });
                    browser.update(cx, |b, _| b.set_shown(true));
                    view = Some(browser.clone());
                    cx.new(|cx| Root::new(browser, window, cx))
                },
            );
            let (Ok(_), Some(view)) = (opened, view) else {
                eprintln!("could not open the window");
                cx.quit();
                return;
            };
            cx.spawn(async move |cx| {
                let pause = Duration::from_millis(2500);
                let steps = [
                    "only-the-worker-knows.invalid:8080".to_string(),
                    format!("localhost:{loopback}"),
                    "file:///etc/hosts".to_string(),
                ];
                for step in steps {
                    view.update(cx, |b, cx| b.go(&step, cx));
                    cx.background_executor().timer(pause).await;
                    let title = view.read_with(cx, |b, _| b.title());
                    println!("{step:<44} title={title:?}");
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    0
}

/// A page server naming the `Host` it was asked for in its title.
fn page_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut host = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(h) = line.strip_prefix("Host: ").or(line.strip_prefix("host: ")) {
                        host = h.trim().to_string();
                    }
                }
                let body = format!("<title>WORKER {host}</title><h1>served for {host}</h1>");
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.shutdown(Shutdown::Both);
            });
        }
    });
    port
}

/// A worker's `GET /tunnel`, minus its checks: every target goes to `page`.
fn stand_in_worker(page: u16) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for mut client in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if client.read(&mut byte).unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let Ok(upstream) = TcpStream::connect(("127.0.0.1", page)) else {
                    return;
                };
                let _ = client.write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\n\
                      Upgrade: flightdeck-tunnel\r\n\r\n",
                );
                let (mut c2, mut u2) = (client.try_clone().unwrap(), upstream.try_clone().unwrap());
                let up = thread::spawn(move || {
                    let _ = std::io::copy(&mut c2, &mut u2);
                    let _ = u2.shutdown(Shutdown::Write);
                });
                let (mut c3, mut u3) = (client, upstream);
                let _ = std::io::copy(&mut u3, &mut c3);
                let _ = c3.shutdown(Shutdown::Write);
                let _ = up.join();
            });
        }
    });
    port
}
