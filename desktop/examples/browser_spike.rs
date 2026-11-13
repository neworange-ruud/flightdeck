//! Spike `remote-control-cefv.1` (`specs/SESSION_BROWSER_PRD.md` §4.1): the
//! system web view inside a GPUI window, behind a SOCKS5 proxy.
//!
//! ```text
//! cargo run -p flightdeck-desktop --example browser_spike
//! ```
//!
//! It answers, by experiment rather than by reading docs:
//!
//! 1. Does a `wry` child web view attach to a GPUI window and render?
//! 2. **Does loopback go through the proxy?** Three servers run here:
//!    - `DESKTOP` on 127.0.0.1:A stands for "this machine's own localhost";
//!    - `SOCKS` on 127.0.0.1:B is the relay, which logs every target it is
//!      asked for and sends them all to
//!    - `WORKER` on 127.0.0.1:C, which stands for the worker.
//!
//!    The web view loads `localhost:A`, `127.0.0.1:A`, `[::1]:A` and a name
//!    only the far side could resolve. A page served by `WORKER` (and a target
//!    in the SOCKS log) means the request went through the proxy; one served
//!    by `DESKTOP` means the engine went direct.
//! 3. Does a proxy combine with a per-session data store (macOS
//!    `WKWebsiteDataStore(forIdentifier:)`)?
//! 4. Does hiding the view (for a GPUI overlay) work?
//!
//! It prints one line per URL and what each server saw, then quits.
//!
//! The first argument picks how the proxy is configured:
//!
//! - `wry` (default): `wry`'s own `with_proxy_config`.
//! - `match`: our own Network.framework configuration on the web view's data
//!   store, with `localhost`, `127.0.0.1` and `::1` as match domains — does
//!   that pull loopback through the proxy?
//! - `auth`: our own configuration with a username and password, which the
//!   relay then requires (RFC 1929).
//! - `loopback` (Windows): WebView2 launched with
//!   `--proxy-bypass-list=<-loopback>`, Chromium's way of sending loopback
//!   through the proxy too. `wry` ignores its own proxy setting once browser
//!   arguments are given, so this passes the whole argument line itself.

// The session browser is macOS and Windows only (B7); wry is not built on Linux.
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod spike {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use gpui::{
        div, px, size, App, AppContext, Bounds, Context, IntoElement, ParentElement, Render,
        Styled, Window, WindowBounds, WindowOptions,
    };
    use wry::dpi::{LogicalPosition, LogicalSize};
    use wry::{PageLoadEvent, ProxyConfig, ProxyEndpoint, Rect, WebView, WebViewBuilder};

    /// What each server saw, in order.
    #[derive(Default)]
    struct Log {
        desktop: Vec<String>,
        worker: Vec<String>,
        socks: Vec<String>,
        loaded: Vec<String>,
        titles: Vec<String>,
    }

    type Shared = Arc<Mutex<Log>>;

    /// A one-page HTTP server that answers every request with `who` and the Host
    /// header it was sent, in the page title.
    fn http_server(who: &'static str, log: Shared) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind http");
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = log.clone();
                thread::spawn(move || serve_http(who, stream, log));
            }
        });
        port
    }

    fn serve_http(who: &'static str, mut stream: TcpStream, log: Shared) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut host = String::new();
        let mut first = String::new();
        if reader.read_line(&mut first).is_err() {
            return;
        }
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            if let Some(h) = line.strip_prefix("Host: ").or(line.strip_prefix("host: ")) {
                host = h.trim().to_string();
            }
        }
        {
            let mut log = log.lock().unwrap();
            let entry = format!("{} (Host: {host})", first.trim());
            match who {
                "DESKTOP" => log.desktop.push(entry),
                _ => log.worker.push(entry),
            }
        }
        let body = format!(
            "<html><head><title>{who} {host}</title></head>\
             <body style='font:24px system-ui;padding:24px'>served by <b>{who}</b> for Host {host}\
             <script>try {{ new WebSocket('ws://websocket-only.invalid:8081/hmr'); }} catch (e) {{}}</script>\
             </body></html>"
        );
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.shutdown(Shutdown::Both);
    }

    /// A minimal SOCKS5 relay (no auth, CONNECT only). It logs the target it is
    /// asked for — the name as the browser gave it, not resolved — and connects
    /// every request to the stand-in worker on `worker_port`.
    fn socks_server(worker_port: u16, log: Shared, auth: bool) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind socks");
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = log.clone();
                thread::spawn(move || {
                    if let Err(e) = serve_socks(stream, worker_port, &log, auth) {
                        log.lock().unwrap().socks.push(format!("error: {e}"));
                    }
                });
            }
        });
        port
    }

    fn serve_socks(
        mut client: TcpStream,
        worker_port: u16,
        log: &Shared,
        auth: bool,
    ) -> std::io::Result<()> {
        let mut head = [0u8; 2];
        client.read_exact(&mut head)?;
        let mut methods = vec![0u8; head[1] as usize];
        client.read_exact(&mut methods)?;
        if auth {
            if !methods.contains(&2) {
                client.write_all(&[5, 0xff])?;
                log.lock().unwrap().socks.push(format!(
                    "refused: no user/pass method offered ({methods:?})"
                ));
                return Ok(());
            }
            client.write_all(&[5, 2])?;
            let mut ver_ulen = [0u8; 2];
            client.read_exact(&mut ver_ulen)?;
            let mut user = vec![0u8; ver_ulen[1] as usize];
            client.read_exact(&mut user)?;
            let mut plen = [0u8; 1];
            client.read_exact(&mut plen)?;
            let mut pass = vec![0u8; plen[0] as usize];
            client.read_exact(&mut pass)?;
            let ok = user == b"fd-user" && pass == b"fd-secret";
            log.lock().unwrap().socks.push(format!(
                "auth user={} ok={ok}",
                String::from_utf8_lossy(&user)
            ));
            client.write_all(&[1, if ok { 0 } else { 1 }])?;
            if !ok {
                return Ok(());
            }
        } else {
            client.write_all(&[5, 0])?;
        }
        let mut req = [0u8; 4];
        client.read_exact(&mut req)?;
        let target = match req[3] {
            1 => {
                let mut a = [0u8; 4];
                client.read_exact(&mut a)?;
                format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
            }
            3 => {
                let mut len = [0u8; 1];
                client.read_exact(&mut len)?;
                let mut name = vec![0u8; len[0] as usize];
                client.read_exact(&mut name)?;
                format!("name {}", String::from_utf8_lossy(&name))
            }
            4 => {
                let mut a = [0u8; 16];
                client.read_exact(&mut a)?;
                format!("{}", std::net::Ipv6Addr::from(a))
            }
            other => format!("atyp {other}"),
        };
        let mut p = [0u8; 2];
        client.read_exact(&mut p)?;
        let target_port = u16::from_be_bytes(p);
        log.lock()
            .unwrap()
            .socks
            .push(format!("CONNECT {target}:{target_port}"));
        let upstream = TcpStream::connect(("127.0.0.1", worker_port))?;
        client.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])?;
        let (mut c2, mut u2) = (client.try_clone()?, upstream.try_clone()?);
        let up = thread::spawn(move || {
            let _ = std::io::copy(&mut c2, &mut u2);
            let _ = u2.shutdown(Shutdown::Write);
        });
        let (mut c3, mut u3) = (client, upstream);
        let _ = std::io::copy(&mut u3, &mut c3);
        let _ = c3.shutdown(Shutdown::Write);
        let _ = up.join();
        Ok(())
    }

    struct Spike {
        webview: WebView,
    }

    impl Render for Spike {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .bg(gpui::rgb(0x1e1e2e))
                .text_color(gpui::rgb(0xcdd6f4))
                .child(div().p_2().child("GPUI chrome — the web view is below"))
        }
    }

    /// Our own Network.framework proxy configuration on the web view's data
    /// store (`WKWebsiteDataStore.proxyConfigurations`, macOS 14+).
    #[cfg(target_os = "macos")]
    mod own_proxy {
        use std::ffi::{c_char, c_void, CString};

        use objc2::rc::Retained;
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};

        type NwObject = *mut c_void;

        #[link(name = "Network", kind = "framework")]
        extern "C" {
            fn nw_endpoint_create_host(hostname: *const c_char, port: *const c_char) -> NwObject;
            fn nw_proxy_config_create_socksv5(endpoint: NwObject) -> NwObject;
            fn nw_proxy_config_add_match_domain(config: NwObject, domain: *const c_char);
            fn nw_proxy_config_set_username_and_password(
                config: NwObject,
                username: *const c_char,
                password: *const c_char,
            );
        }

        pub fn install(
            webview: &wry::WebView,
            port: u16,
            match_domains: &[&str],
            credentials: Option<(&str, &str)>,
        ) {
            use wry::WebViewExtMacOS;
            let host = CString::new("127.0.0.1").unwrap();
            let port = CString::new(port.to_string()).unwrap();
            unsafe {
                let endpoint = nw_endpoint_create_host(host.as_ptr(), port.as_ptr());
                let config = nw_proxy_config_create_socksv5(endpoint);
                for domain in match_domains {
                    let domain = CString::new(*domain).unwrap();
                    nw_proxy_config_add_match_domain(config, domain.as_ptr());
                }
                if let Some((user, pass)) = credentials {
                    let user = CString::new(user).unwrap();
                    let pass = CString::new(pass).unwrap();
                    nw_proxy_config_set_username_and_password(config, user.as_ptr(), pass.as_ptr());
                }
                let wk = webview.webview();
                let wk: &AnyObject = &wk;
                let configuration: Retained<AnyObject> = msg_send![wk, configuration];
                let store: Retained<AnyObject> = msg_send![&*configuration, websiteDataStore];
                let array: Retained<AnyObject> =
                    msg_send![class!(NSArray), arrayWithObject: config as *mut AnyObject];
                let key: Retained<AnyObject> = msg_send![
                    class!(NSString),
                    stringWithUTF8String: c"proxyConfigurations".as_ptr()
                ];
                let _: () = msg_send![&*store, setValue: &*array, forKey: &*key];
            }
        }
    }

    pub fn main() {
        let mode = std::env::args().nth(1).unwrap_or_else(|| "wry".to_string());
        println!("mode: {mode}");
        let log: Shared = Arc::default();
        let desktop = http_server("DESKTOP", log.clone());
        let worker = http_server("WORKER", log.clone());
        let socks = socks_server(worker, log.clone(), mode == "auth");
        println!("DESKTOP 127.0.0.1:{desktop}  WORKER 127.0.0.1:{worker}  SOCKS 127.0.0.1:{socks}");

        let urls = vec![
            format!("http://localhost:{desktop}/"),
            format!("http://127.0.0.1:{desktop}/"),
            format!("http://[::1]:{desktop}/"),
            "http://only-the-worker-knows.invalid:8080/".to_string(),
            "http://example.com/".to_string(),
        ];

        gpui_platform::application().run(move |cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(900.), px(640.)), cx);
            let log_for_view = log.clone();
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        ..Default::default()
                    },
                    move |window, cx| {
                        let load_log = log_for_view.clone();
                        let title_log = log_for_view.clone();
                        let mut builder = WebViewBuilder::new().with_bounds(Rect {
                            position: LogicalPosition::new(0, 40).into(),
                            size: LogicalSize::new(900, 600).into(),
                        });
                        if mode == "wry" {
                            builder =
                                builder.with_proxy_config(ProxyConfig::Socks5(ProxyEndpoint {
                                    host: "127.0.0.1".to_string(),
                                    port: socks.to_string(),
                                }));
                        }
                        let builder = builder
                            .with_on_page_load_handler(move |event, url| {
                                if matches!(event, PageLoadEvent::Finished) {
                                    load_log.lock().unwrap().loaded.push(url);
                                }
                            })
                            .with_document_title_changed_handler(move |title| {
                                title_log.lock().unwrap().titles.push(title);
                            });
                        #[cfg(target_os = "windows")]
                        let builder = if mode == "loopback" {
                            use wry::WebViewBuilderExtWindows;
                            builder.with_additional_browser_args(format!(
                                "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
                                 --proxy-server=socks5://127.0.0.1:{socks} \
                                 --proxy-bypass-list=<-loopback>"
                            ))
                        } else {
                            builder
                        };
                        #[cfg(target_os = "macos")]
                        let builder = {
                            use wry::WebViewBuilderExtDarwin;
                            builder.with_data_store_identifier(*b"flightdeck-spike")
                        };
                        let webview = builder.build_as_child(window).expect("build the web view");
                        #[cfg(target_os = "macos")]
                        match mode.as_str() {
                            "match" => own_proxy::install(
                                &webview,
                                socks,
                                &["localhost", "127.0.0.1", "::1"],
                                None,
                            ),
                            "auth" => own_proxy::install(
                                &webview,
                                socks,
                                &[],
                                Some(("fd-user", "fd-secret")),
                            ),
                            _ => {}
                        }
                        cx.new(|_| Spike { webview })
                    },
                )
                .expect("open the window");

            cx.spawn(async move |cx| {
                let pause = Duration::from_millis(2500);
                for url in &urls {
                    let _ = window.update(cx, |spike, _, _| spike.webview.load_url(url));
                    cx.background_executor().timer(pause).await;
                    let log = log.lock().unwrap();
                    println!(
                        "{url:<48} title={:?}",
                        log.titles.last().cloned().unwrap_or_default()
                    );
                }
                // Hiding (for a GPUI overlay) and showing again.
                let hidden = window.update(cx, |spike, _, _| {
                    (
                        spike.webview.set_visible(false),
                        spike.webview.set_visible(true),
                    )
                });
                println!("set_visible(false/true): {hidden:?}");

                let log = log.lock().unwrap();
                println!("\nDESKTOP saw: {:#?}", log.desktop);
                println!("WORKER saw:  {:#?}", log.worker);
                println!("SOCKS saw:   {:#?}", log.socks);
                println!("pages loaded: {:#?}", log.loaded);
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    }
}

fn main() {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    spike::main();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    eprintln!("browser_spike runs on macOS and Windows only");
}
