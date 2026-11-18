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
            // A local session's browser, for the certificate step: this
            // computer's own network, as a dev server on it is reached.
            let mut direct = None;
            let _ = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    let browser = cx.new(|cx| {
                        BrowserView::new_direct(super::profile_id("smoke", "direct"), window, cx)
                    });
                    browser.update(cx, |b, _| b.set_shown(true));
                    direct = Some(browser.clone());
                    cx.new(|cx| Root::new(browser, window, cx))
                },
            );
            let Some(direct) = direct else {
                eprintln!("could not open the second window");
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
                // B21: a self-signed HTTPS server, its certificate page, and
                // Continue anyway. Needs openssl to make the server.
                let tls = self_signed_server();
                if tls.is_none() {
                    println!("{:<44} skipped: no openssl", "self-signed certificate");
                }
                if let Some((server, port)) = tls {
                    let step = format!("https://localhost:{port}/");
                    direct.update(cx, |b, cx| b.go(&step, cx));
                    cx.background_executor().timer(pause).await;
                    let failed = direct.read_with(cx, |b, _| b.certificate_failure());
                    println!("{step:<44} certificate failure={failed:?}");
                    direct.update(cx, |b, _| b.continue_anyway());
                    cx.background_executor().timer(pause).await;
                    let (url, still) =
                        direct.read_with(cx, |b, _| (b.url(), b.certificate_failure()));
                    println!("{:<44} url={url:?} failure={still:?}", "continue anyway");
                    drop(server);
                    super::trust::TrustStore::at(super::trust::default_path())
                        .forget(&super::profile_id("smoke", "direct"));
                }
                // The keyboard stays GPUI's: a page that focuses a field as
                // it loads, and DevTools opening, both take the window's
                // first responder; nobody clicked them, so it comes back.
                #[cfg(target_os = "macos")]
                {
                    let (server, port) = autofocus_server();
                    direct.update(cx, |b, cx| b.go(&format!("localhost:{port}"), cx));
                    cx.background_executor().timer(pause).await;
                    let page = direct.read_with(cx, |b, _| b.page_has_keyboard());
                    println!("{:<44} page has the keyboard={page}", "autofocus page");
                    direct.update(cx, |b, _| b.open_devtools());
                    cx.background_executor().timer(pause).await;
                    let page = direct.read_with(cx, |b, _| b.page_has_keyboard());
                    println!("{:<44} page has the keyboard={page}", "devtools opened");
                    drop(server);
                }
                // A local session on Windows goes through the hub's direct
                // route: this computer's localhost, by way of the proxy.
                #[cfg(target_os = "windows")]
                {
                    let (server, port) = autofocus_server();
                    let step = format!("localhost:{port}");
                    direct.update(cx, |b, cx| b.go(&step, cx));
                    cx.background_executor().timer(pause).await;
                    let title = direct.read_with(cx, |b, _| b.title());
                    println!("{:<44} title={title:?}", format!("{step} (local session)"));
                    drop(server);
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    0
}

/// A page whose text field takes the focus as it loads.
fn autofocus_server() -> (thread::JoinHandle<()>, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        for mut stream in listener.incoming().flatten().take(4) {
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let body = "<!doctype html><title>autofocus</title><input autofocus>\
                        <script>document.querySelector('input').focus()</script>";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (handle, port)
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

/// `openssl s_server -www` on a free port with a fresh self-signed
/// certificate for `localhost`, killed when the guard drops.
fn self_signed_server() -> Option<(ServerGuard, u16)> {
    let dir = std::env::temp_dir().join(format!("fd-spike-tls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    let openssl = openssl()?;
    let made = std::process::Command::new(&openssl)
        .args([
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        ])
        .args(["-subj", "/CN=localhost"])
        .arg("-keyout")
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !made.success() {
        return None;
    }
    let free = TcpListener::bind("127.0.0.1:0").ok()?;
    let port = free.local_addr().ok()?.port();
    drop(free);
    let child = std::process::Command::new(&openssl)
        .args(["s_server", "-www", "-quiet", "-accept"])
        .arg(port.to_string())
        .arg("-cert")
        .arg(&cert)
        .arg("-key")
        .arg(&key)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    thread::sleep(Duration::from_millis(500));
    println!("self-signed https server on localhost:{port}");
    Some((ServerGuard(child), port))
}

/// Kills the server it holds.
struct ServerGuard(std::process::Child);

/// An `openssl` to run: on the path, else (Windows) Git for Windows' own.
fn openssl() -> Option<std::path::PathBuf> {
    let on_path = std::process::Command::new("openssl")
        .arg("version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if on_path {
        return Some("openssl".into());
    }
    #[cfg(target_os = "windows")]
    {
        let git = std::path::PathBuf::from(r"C:\Program Files\Git\usr\bin\openssl.exe");
        if git.exists() {
            return Some(git);
        }
    }
    None
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

/// `flightdeck-desktop --spike-agent-browser`: every agent browser tool
/// (`specs/SESSION_BROWSER_PRD.md` §6) on a real web view, against a form
/// served here, printing each answer, then quitting. A debugging aid, like
/// `--spike-browser`.
pub fn run_agent() -> i32 {
    use flightdeck::browser_tools::{BrowserReply, ReplyContent, Tool};
    use std::cell::RefCell;
    use std::rc::Rc;

    let port = form_server();
    println!("form server 127.0.0.1:{port}");
    gpui_platform::application()
        .with_assets(crate::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            let bounds = Bounds::centered(None, size(px(1000.), px(700.)), cx);
            let mut view = None;
            let _ = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    let browser = cx.new(|cx| {
                        BrowserView::new_direct(super::profile_id("smoke", "agent"), window, cx)
                    });
                    browser.update(cx, |b, cx| {
                        b.set_shown(true);
                        b.set_agent_allowed(true, cx);
                    });
                    view = Some(browser.clone());
                    cx.new(|cx| Root::new(browser, window, cx))
                },
            );
            let Some(view) = view else {
                eprintln!("could not open the window");
                cx.quit();
                return;
            };
            cx.spawn(async move |cx| {
                let finished: super::agent::Finished = Rc::new(RefCell::new(Vec::new()));
                let mut call = 0u64;
                // Run one tool and wait for its answer.
                let mut run = async |tool: Tool, label: &str, cx: &mut gpui::AsyncApp| -> BrowserReply {
                    call += 1;
                    let id = call;
                    let f = finished.clone();
                    view.update(cx, |b, cx| b.agent_run(id, tool, f, cx));
                    loop {
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                        let done = finished.borrow_mut().iter().position(|(c, _)| *c == id);
                        if let Some(at) = done {
                            let (_, reply) = finished.borrow_mut().remove(at);
                            let mut shown = String::new();
                            for part in &reply.content {
                                match part {
                                    ReplyContent::Text { text } => shown.push_str(text),
                                    ReplyContent::Image { data, mime_type } => shown.push_str(
                                        &format!(" [{mime_type}, {} base64 chars]", data.len()),
                                    ),
                                }
                            }
                            println!(
                                "== {label}{}\n{}\n",
                                if reply.is_error { " (error)" } else { "" },
                                shown.lines().take(40).collect::<Vec<_>>().join("\n")
                            );
                            return reply;
                        }
                    }
                };
                let tab = None;
                run(Tool::Navigate { url: format!("localhost:{port}"), tab }, "navigate", cx).await;
                let snap = run(Tool::Snapshot { tab }, "snapshot", cx).await.joined_text();
                let find = |what: &str| {
                    snap.lines()
                        .find(|l| l.contains(what))
                        .and_then(|l| l.split("[ref=").nth(1))
                        .and_then(|r| r.split(']').next())
                        .unwrap_or("e0")
                        .to_string()
                };
                let (email, plan, terms, button) =
                    (find("textbox \"Email"), find("combobox"), find("checkbox"), find("button \"Sign in"));
                run(Tool::Type { r#ref: email, text: "ada@example.com".into(), submit: false, tab }, "type email", cx).await;
                run(Tool::Type { r#ref: plan, text: "Pro".into(), submit: false, tab }, "choose plan", cx).await;
                run(Tool::Click { r#ref: terms, tab }, "tick terms", cx).await;
                run(Tool::Click { r#ref: "e999".into(), tab }, "stale ref", cx).await;
                run(Tool::Console { clear: true, tab }, "console before submit", cx).await;
                run(Tool::Console { clear: false, tab }, "console after clearing", cx).await;
                run(Tool::Click { r#ref: button, tab }, "submit", cx).await;
                run(Tool::Snapshot { tab }, "snapshot after submit", cx).await;
                run(Tool::Console { clear: false, tab }, "console", cx).await;
                run(Tool::Eval { expression: "document.title;".into(), tab }, "eval title", cx).await;
                run(Tool::Eval { expression: "() => new Promise(r => setTimeout(() => r({ ok: location.search }), 300))".into(), tab }, "eval promise", cx).await;
                run(Tool::Eval { expression: "1 +".into(), tab }, "eval syntax error", cx).await;
                run(Tool::Eval { expression: "Promise.reject(new Error('nope'))".into(), tab }, "eval rejected", cx).await;
                let shot = run(Tool::Screenshot { tab }, "screenshot (shown)", cx).await;
                // FD_SMOKE_SHOT=<file.png> keeps it, to look at.
                if let Some(path) = std::env::var_os("FD_SMOKE_SHOT") {
                    use base64::Engine;
                    for part in &shot.content {
                        if let ReplyContent::Image { data, .. } = part {
                            if let Ok(png) = base64::engine::general_purpose::STANDARD.decode(data) {
                                let _ = std::fs::write(&path, png);
                            }
                        }
                    }
                }
                run(Tool::Back { tab }, "back", cx).await;
                run(Tool::Press { key: "Tab".into(), tab }, "press Tab", cx).await;
                run(Tool::Reload { tab }, "reload", cx).await;
                run(Tool::Navigate { url: "file:///etc/hosts".into(), tab }, "navigate to a file", cx).await;
                run(Tool::Navigate { url: "not an address".into(), tab }, "navigate to nonsense", cx).await;
                view.update(cx, |b, _| b.set_shown(false));
                run(Tool::Navigate { url: format!("localhost:{port}"), tab }, "navigate (hidden)", cx).await;
                run(Tool::Screenshot { tab }, "screenshot (hidden)", cx).await;
                let signals = view.update(cx, |b, _| b.take_signals());
                println!("signals: {signals:?}");
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    0
}

/// A sign-in form and the page it submits to, for `--spike-agent-browser`.
fn form_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            let _ = reader.read_line(&mut first);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
            let body = if path.starts_with("/done") {
                format!(
                    "<!doctype html><title>Done</title><h1>Thanks</h1><p>You sent {}</p>",
                    path.replace('<', "&lt;")
                )
            } else {
                "<!doctype html><title>Sign in</title><h1>Sign in</h1>\
                 <p>Welcome to the   test page.</p>\
                 <form action=\"/done\" method=\"get\">\
                 <label>Email <input name=\"email\" placeholder=\"you@example.com\"></label>\
                 <select name=\"plan\"><option value=\"a\">Basic</option><option value=\"b\">Pro</option></select>\
                 <label><input type=\"checkbox\" name=\"terms\"> I agree</label>\
                 <button type=\"submit\">Sign in</button></form>\
                 <nav><a href=\"/about\">About us</a></nav>\
                 <script>console.log('hello from the page', {n: 1}); console.warn('careful');\
                 setTimeout(() => { throw new Error('boom') }, 10); Promise.reject(new Error('nope'));</script>"
                    .to_string()
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.shutdown(Shutdown::Both);
        }
    });
    port
}
