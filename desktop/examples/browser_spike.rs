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
//!
//! Windows only, for `remote-control-cefv.8` (run them all with
//! `scripts/desktop/windows-browser-spike.ps1`):
//!
//! - `http-auth`: an **HTTP** proxy that demands `Proxy-Authorization`
//!   (Chromium cannot send SOCKS5 credentials). WebView2's
//!   `BasicAuthenticationRequested` event answers with a username and
//!   password: does it fire for a proxy's 407, and does the proxy then see
//!   them? If yes, the relay can keep other programs out the way it does on
//!   macOS (B23).
//! - `pid`: the SOCKS relay looks up which process opened each connection
//!   (`GetExtendedTcpTable`) and whether that process descends from this one.
//!   It then waits for a connection from outside (the script sends one with
//!   `curl.exe`), which must come out as foreign. If WebView2's network
//!   process is ours and curl's is not, the relay can refuse by process.
//! - `profiles`: a profile per session (B10) with a proxy. Two web views
//!   share one data folder and one proxy, in profiles `session-a` and
//!   `session-b`: a cookie and `localStorage` set in one must not show in the
//!   other. A third asks for the same data folder with a **different** proxy
//!   port (each session has its own relay, so its own port): does WebView2
//!   refuse? A fourth uses a data folder of its own with that port.

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
        /// Everything else worth reporting: the proxy-login event, web views
        /// that could not be built.
        notes: Vec<String>,
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
             <script>try {{ new WebSocket('ws://websocket-only.invalid:8081/hmr'); }} catch (e) {{}}\
             try {{ document.title += ' cookie=' + (document.cookie || '-') + ' storage=' + \
             (localStorage.getItem('seen') || '-'); document.cookie = 'seen=1; path=/'; \
             localStorage.setItem('seen', '1'); }} catch (e) {{ document.title += ' ' + e; }}</script>\
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
    fn socks_server(label: &'static str, worker_port: u16, log: Shared, auth: bool) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind socks");
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = log.clone();
                thread::spawn(move || {
                    if let Err(e) = serve_socks(label, stream, worker_port, &log, auth) {
                        log.lock()
                            .unwrap()
                            .socks
                            .push(format!("{label} error: {e}"));
                    }
                });
            }
        });
        port
    }

    fn serve_socks(
        label: &'static str,
        mut client: TcpStream,
        worker_port: u16,
        log: &Shared,
        auth: bool,
    ) -> std::io::Result<()> {
        #[cfg(target_os = "windows")]
        if let Ok(peer) = client.peer_addr() {
            let who = windows_only::describe_client(peer);
            log.lock()
                .unwrap()
                .socks
                .push(format!("{label} connection from {who}"));
        }
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
            .push(format!("{label} CONNECT {target}:{target_port}"));
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
        /// The first is the one the URLs load in; `profiles` adds more.
        webviews: Vec<(&'static str, WebView)>,
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

    /// An HTTP proxy that demands `Proxy-Authorization: Basic` for
    /// `fd-user` / `fd-secret`, logs what it was asked and with which
    /// credentials, and sends everything to the stand-in worker. `CONNECT`
    /// for HTTPS and WebSockets, absolute-form requests for plain HTTP.
    #[cfg(target_os = "windows")]
    fn http_proxy_server(worker_port: u16, log: Shared) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind http proxy");
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = log.clone();
                thread::spawn(move || {
                    if let Err(e) = serve_http_proxy(stream, worker_port, &log) {
                        log.lock()
                            .unwrap()
                            .socks
                            .push(format!("HTTP-PROXY error: {e}"));
                    }
                });
            }
        });
        port
    }

    #[cfg(target_os = "windows")]
    fn serve_http_proxy(client: TcpStream, worker_port: u16, log: &Shared) -> std::io::Result<()> {
        // base64("fd-user:fd-secret")
        const EXPECTED: &str = "Basic ZmQtdXNlcjpmZC1zZWNyZXQ=";
        let mut reader = BufReader::new(client.try_clone()?);
        let mut first = String::new();
        reader.read_line(&mut first)?;
        let mut head = Vec::new();
        let mut auth = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 || line == "\r\n" {
                break;
            }
            match line.split_once(':') {
                Some((name, value)) if name.eq_ignore_ascii_case("proxy-authorization") => {
                    auth = Some(value.trim().to_string());
                }
                _ => head.push(line),
            }
        }
        let verdict = match &auth {
            None => "none",
            Some(a) if a == EXPECTED => "ok",
            Some(_) => "WRONG",
        };
        log.lock()
            .unwrap()
            .socks
            .push(format!("HTTP-PROXY {} auth={verdict}", first.trim()));
        let mut client = client;
        if verdict != "ok" {
            client.write_all(
                b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                  Proxy-Authenticate: Basic realm=\"flightdeck\"\r\n\
                  Content-Length: 0\r\nConnection: close\r\n\r\n",
            )?;
            return Ok(());
        }
        let mut upstream = TcpStream::connect(("127.0.0.1", worker_port))?;
        if first.starts_with("CONNECT ") {
            client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")?;
        } else {
            upstream.write_all(first.as_bytes())?;
            for line in &head {
                upstream.write_all(line.as_bytes())?;
            }
            upstream.write_all(b"\r\n")?;
        }
        let (mut c2, mut u2) = (client.try_clone()?, upstream.try_clone()?);
        let up = thread::spawn(move || {
            let _ = std::io::copy(&mut c2, &mut u2);
            let _ = u2.shutdown(Shutdown::Write);
        });
        let _ = std::io::copy(&mut upstream, &mut client);
        let _ = client.shutdown(Shutdown::Write);
        let _ = up.join();
        Ok(())
    }

    /// Windows-only probes: WebView2's proxy-login event, and which process
    /// opened a connection.
    #[cfg(target_os = "windows")]
    mod windows_only {
        use std::collections::HashMap;
        use std::net::{IpAddr, SocketAddr};

        use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2, ICoreWebView2_10};
        use webview2_com::{take_pwstr, BasicAuthenticationRequestedEventHandler};
        use windows::core::{Interface, HSTRING, PWSTR};
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_CONNECTIONS,
        };
        use windows::Win32::Networking::WinSock::AF_INET;
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };
        use windows::Win32::System::Threading::GetCurrentProcessId;

        use super::Shared;

        /// Answer every authentication request — a page's or a proxy's —
        /// with `fd-user` / `fd-secret`, and note each one.
        pub fn answer_proxy_login(webview: &wry::WebView, log: Shared) {
            use wry::WebViewExtWindows;
            let core: ICoreWebView2 = webview.webview();
            let core: ICoreWebView2_10 = match core.cast() {
                Ok(core) => core,
                Err(e) => {
                    log.lock()
                        .unwrap()
                        .notes
                        .push(format!("no ICoreWebView2_10 (runtime too old?): {e}"));
                    return;
                }
            };
            let events = log.clone();
            let handler =
                BasicAuthenticationRequestedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    // SAFETY: WebView2 COM calls on its own callback's
                    // arguments, on the UI thread it calls us on.
                    unsafe {
                        let mut uri = PWSTR::null();
                        args.Uri(&mut uri)?;
                        let mut challenge = PWSTR::null();
                        args.Challenge(&mut challenge)?;
                        events.lock().unwrap().notes.push(format!(
                            "BasicAuthenticationRequested uri={} challenge={}",
                            take_pwstr(uri),
                            take_pwstr(challenge)
                        ));
                        let response = args.Response()?;
                        response.SetUserName(&HSTRING::from("fd-user"))?;
                        response.SetPassword(&HSTRING::from("fd-secret"))?;
                    }
                    Ok(())
                }));
            let mut token = 0i64;
            // SAFETY: registering a handler WebView2 keeps alive itself.
            if let Err(e) = unsafe { core.add_BasicAuthenticationRequested(&handler, &mut token) } {
                log.lock()
                    .unwrap()
                    .notes
                    .push(format!("add_BasicAuthenticationRequested failed: {e}"));
            }
        }

        /// The process at the other end of a connection the relay accepted
        /// from `peer`, its parents, and whether it descends from this
        /// process.
        pub fn describe_client(peer: SocketAddr) -> String {
            let Some(pid) = owning_pid(peer) else {
                return format!("{peer}: owning process not found");
            };
            let processes = processes();
            let me = unsafe { GetCurrentProcessId() };
            let mut chain = Vec::new();
            let mut ours = false;
            let mut at = pid;
            for _ in 0..16 {
                if at == me {
                    ours = true;
                }
                match processes.get(&at) {
                    Some((parent, name)) => {
                        chain.push(format!("{name}({at})"));
                        if *parent == 0 || *parent == at {
                            break;
                        }
                        at = *parent;
                    }
                    None => {
                        chain.push(format!("?({at})"));
                        break;
                    }
                }
            }
            format!(
                "{peer}: {} — {}",
                chain.join(" <- "),
                if ours {
                    "OURS (descends from this process)"
                } else {
                    "FOREIGN (not a descendant of this process)"
                }
            )
        }

        /// The process owning the IPv4 TCP connection whose local end is
        /// `peer`.
        fn owning_pid(peer: SocketAddr) -> Option<u32> {
            let IpAddr::V4(ip) = peer.ip() else {
                return None;
            };
            let mut size = 0u32;
            // SAFETY: the first call only reports the size needed; the
            // second fills a buffer of that size, read as the documented
            // MIB_TCPTABLE_OWNER_PID (a count, then the rows).
            unsafe {
                GetExtendedTcpTable(
                    None,
                    &mut size,
                    false,
                    AF_INET.0 as u32,
                    TCP_TABLE_OWNER_PID_CONNECTIONS,
                    0,
                );
                let mut buffer = vec![0u32; (size as usize).div_ceil(4) + 1];
                let status = GetExtendedTcpTable(
                    Some(buffer.as_mut_ptr().cast()),
                    &mut size,
                    false,
                    AF_INET.0 as u32,
                    TCP_TABLE_OWNER_PID_CONNECTIONS,
                    0,
                );
                if status != 0 {
                    return None;
                }
                let count = buffer[0] as usize;
                let rows = std::slice::from_raw_parts(
                    buffer.as_ptr().add(1).cast::<MIB_TCPROW_OWNER_PID>(),
                    count,
                );
                rows.iter()
                    .find(|row| {
                        std::net::Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes()) == ip
                            && u16::from_be(row.dwLocalPort as u16) == peer.port()
                    })
                    .map(|row| row.dwOwningPid)
            }
        }

        /// Every process: its parent and its executable's name.
        fn processes() -> HashMap<u32, (u32, String)> {
            let mut out = HashMap::new();
            // SAFETY: a process snapshot walked with a correctly sized
            // PROCESSENTRY32W, closed afterwards.
            unsafe {
                let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                    return out;
                };
                let mut entry = PROCESSENTRY32W {
                    dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                    ..Default::default()
                };
                let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
                while more {
                    let len = entry
                        .szExeFile
                        .iter()
                        .position(|c| *c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                    out.insert(entry.th32ProcessID, (entry.th32ParentProcessID, name));
                    more = Process32NextW(snapshot, &mut entry).is_ok();
                }
                let _ = CloseHandle(snapshot);
            }
            out
        }
    }

    /// Where a web view goes in the window, and how it is set up.
    struct Plan {
        label: &'static str,
        bounds: (i32, i32, u32, u32),
        /// The `wry` mode's own proxy setting, on this SOCKS port.
        wry_proxy: Option<u16>,
        /// WebView2's whole browser-argument line (Windows).
        #[allow(dead_code)]
        browser_args: Option<String>,
        /// WebView2 profile name (Windows).
        #[allow(dead_code)]
        profile: Option<&'static str>,
        /// WebView2 user data folder (Windows); `None` is wry's default.
        #[allow(dead_code)]
        data_dir: Option<std::path::PathBuf>,
    }

    /// WebView2's arguments for a proxy at `proxy` that loopback goes
    /// through too.
    #[allow(dead_code)]
    fn chromium_args(proxy: &str) -> String {
        format!(
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
             --proxy-server={proxy} --proxy-bypass-list=<-loopback>"
        )
    }

    pub fn main() {
        let mode = std::env::args().nth(1).unwrap_or_else(|| "wry".to_string());
        println!("mode: {mode}");
        let log: Shared = Arc::default();
        let desktop = http_server("DESKTOP", log.clone());
        let worker = http_server("WORKER", log.clone());
        let socks = socks_server("SOCKS", worker, log.clone(), mode == "auth");
        println!("DESKTOP 127.0.0.1:{desktop}  WORKER 127.0.0.1:{worker}  SOCKS 127.0.0.1:{socks}");

        let full = (0, 40, 900, 600);
        // Only Windows' modes change the plan.
        #[cfg_attr(not(target_os = "windows"), allow(unused_mut))]
        let mut plans = vec![Plan {
            label: "main",
            bounds: full,
            wry_proxy: (mode == "wry").then_some(socks),
            browser_args: None,
            profile: None,
            data_dir: None,
        }];
        #[cfg(target_os = "windows")]
        match mode.as_str() {
            "loopback" | "pid" => {
                plans[0].browser_args = Some(chromium_args(&format!("socks5://127.0.0.1:{socks}")));
            }
            "http-auth" => {
                let proxy = http_proxy_server(worker, log.clone());
                println!("HTTP-PROXY 127.0.0.1:{proxy} (wants fd-user / fd-secret)");
                plans[0].browser_args = Some(chromium_args(&format!("http://127.0.0.1:{proxy}")));
            }
            "profiles" => {
                let second = socks_server("SOCKS2", worker, log.clone(), false);
                println!("SOCKS2 127.0.0.1:{second}");
                let base = std::env::temp_dir()
                    .join(format!("flightdeck-browser-spike-{}", std::process::id()));
                let shared = base.join("shared");
                let own = base.join("own");
                let args1 = chromium_args(&format!("socks5://127.0.0.1:{socks}"));
                let args2 = chromium_args(&format!("socks5://127.0.0.1:{second}"));
                let half = |x, y| (x, y, 450, 300);
                plans = vec![
                    Plan {
                        label: "A shared/session-a",
                        bounds: half(0, 40),
                        wry_proxy: None,
                        browser_args: Some(args1.clone()),
                        profile: Some("session-a"),
                        data_dir: Some(shared.clone()),
                    },
                    Plan {
                        label: "B shared/session-b",
                        bounds: half(450, 40),
                        wry_proxy: None,
                        browser_args: Some(args1),
                        profile: Some("session-b"),
                        data_dir: Some(shared.clone()),
                    },
                    Plan {
                        label: "C shared/other-port",
                        bounds: half(0, 340),
                        wry_proxy: None,
                        browser_args: Some(args2.clone()),
                        profile: Some("session-c"),
                        data_dir: Some(shared),
                    },
                    Plan {
                        label: "D own/other-port",
                        bounds: half(450, 340),
                        wry_proxy: None,
                        browser_args: Some(args2),
                        profile: Some("session-d"),
                        data_dir: Some(own),
                    },
                ];
            }
            _ => {}
        }

        // (web view label, URL), in order.
        let worker_page = "http://only-the-worker-knows.invalid:8080/".to_string();
        let steps: Vec<(&'static str, String)> = if mode == "profiles" {
            vec![
                ("A shared/session-a", worker_page.clone()),
                ("A shared/session-a", worker_page.clone()),
                ("B shared/session-b", worker_page.clone()),
                ("C shared/other-port", worker_page.clone()),
                ("D own/other-port", worker_page.clone()),
                ("D own/other-port", format!("http://localhost:{desktop}/")),
            ]
        } else {
            [
                format!("http://localhost:{desktop}/"),
                format!("http://127.0.0.1:{desktop}/"),
                format!("http://[::1]:{desktop}/"),
                worker_page,
                "http://example.com/".to_string(),
            ]
            .into_iter()
            .map(|url| ("main", url))
            .collect()
        };

        gpui_platform::application().run(move |cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(900.), px(640.)), cx);
            let log_for_view = log.clone();
            let mode_for_view = mode.clone();
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        ..Default::default()
                    },
                    move |window, cx| {
                        let mut webviews = Vec::new();
                        for plan in plans {
                            match build(&plan, &mode_for_view, socks, &log_for_view, window) {
                                Ok(webview) => webviews.push((plan.label, webview)),
                                Err(e) => log_for_view
                                    .lock()
                                    .unwrap()
                                    .notes
                                    .push(format!("{}: could not build: {e}", plan.label)),
                            }
                        }
                        cx.new(|_| Spike { webviews })
                    },
                )
                .expect("open the window");

            let port_file = std::env::temp_dir().join("flightdeck-browser-spike-socks-port");
            let wait_for_foreign = mode == "pid";
            cx.spawn(async move |cx| {
                let pause = Duration::from_millis(2500);
                for (label, url) in &steps {
                    let loaded = window.update(cx, |spike, _, _| {
                        spike
                            .webviews
                            .iter()
                            .find(|(l, _)| l == label)
                            .map(|(_, webview)| webview.load_url(url))
                    });
                    if !matches!(loaded, Ok(Some(_))) {
                        println!("{label:<22} {url:<44} (no such web view)");
                        continue;
                    }
                    cx.background_executor().timer(pause).await;
                    let log = log.lock().unwrap();
                    let prefix = format!("{label}: ");
                    let title = log
                        .titles
                        .iter()
                        .rev()
                        .find_map(|t| t.strip_prefix(&prefix))
                        .unwrap_or_default()
                        .to_string();
                    println!("{label:<22} {url:<44} title={title:?}");
                }
                // Hiding (for a GPUI overlay) and showing again.
                let hidden = window.update(cx, |spike, _, _| {
                    spike.webviews.first().map(|(_, webview)| {
                        (webview.set_visible(false), webview.set_visible(true))
                    })
                });
                println!("set_visible(false/true): {hidden:?}");

                if wait_for_foreign {
                    // The script connects with curl.exe now: a program that
                    // is not this one.
                    let _ = std::fs::write(&port_file, socks.to_string());
                    println!(
                        "waiting 20s for a connection from another program on SOCKS 127.0.0.1:{socks}"
                    );
                    cx.background_executor()
                        .timer(Duration::from_secs(20))
                        .await;
                    let _ = std::fs::remove_file(&port_file);
                }

                let log = log.lock().unwrap();
                println!("\nDESKTOP saw: {:#?}", log.desktop);
                println!("WORKER saw:  {:#?}", log.worker);
                println!("PROXY saw:   {:#?}", log.socks);
                println!("pages loaded: {:#?}", log.loaded);
                println!("notes:       {:#?}", log.notes);
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    }

    /// Build one web view as `plan` says, its titles logged under its label.
    fn build(
        plan: &Plan,
        mode: &str,
        socks: u16,
        log: &Shared,
        window: &mut Window,
    ) -> wry::Result<WebView> {
        let _ = socks;
        let load_log = log.clone();
        let title_log = log.clone();
        let label = plan.label;
        let (x, y, w, h) = plan.bounds;
        #[cfg(target_os = "windows")]
        let mut context = plan
            .data_dir
            .clone()
            .map(|dir| wry::WebContext::new(Some(dir)));
        #[cfg(target_os = "windows")]
        let builder = match context.as_mut() {
            Some(context) => WebViewBuilder::new_with_web_context(context),
            None => WebViewBuilder::new(),
        };
        #[cfg(not(target_os = "windows"))]
        let builder = WebViewBuilder::new();
        let mut builder = builder.with_bounds(Rect {
            position: LogicalPosition::new(x, y).into(),
            size: LogicalSize::new(w, h).into(),
        });
        if let Some(port) = plan.wry_proxy {
            builder = builder.with_proxy_config(ProxyConfig::Socks5(ProxyEndpoint {
                host: "127.0.0.1".to_string(),
                port: port.to_string(),
            }));
        }
        let builder = builder
            .with_on_page_load_handler(move |event, url| {
                if matches!(event, PageLoadEvent::Finished) {
                    load_log
                        .lock()
                        .unwrap()
                        .loaded
                        .push(format!("{label}: {url}"));
                }
            })
            .with_document_title_changed_handler(move |title| {
                title_log
                    .lock()
                    .unwrap()
                    .titles
                    .push(format!("{label}: {title}"));
            });
        #[cfg(target_os = "windows")]
        let builder = {
            use wry::WebViewBuilderExtWindows;
            let mut builder = builder;
            if let Some(args) = &plan.browser_args {
                builder = builder.with_additional_browser_args(args.clone());
            }
            if let Some(profile) = plan.profile {
                builder = builder.with_profile_name(profile);
            }
            builder
        };
        #[cfg(target_os = "macos")]
        let builder = {
            use wry::WebViewBuilderExtDarwin;
            builder.with_data_store_identifier(*b"flightdeck-spike")
        };
        let webview = builder.build_as_child(window)?;
        #[cfg(target_os = "macos")]
        match mode {
            "match" => {
                own_proxy::install(&webview, socks, &["localhost", "127.0.0.1", "::1"], None)
            }
            "auth" => own_proxy::install(&webview, socks, &[], Some(("fd-user", "fd-secret"))),
            _ => {}
        }
        #[cfg(target_os = "windows")]
        if mode == "http-auth" {
            windows_only::answer_proxy_login(&webview, log.clone());
        }
        let _ = mode;
        Ok(webview)
    }
}

fn main() {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    spike::main();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    eprintln!("browser_spike runs on macOS and Windows only");
}
