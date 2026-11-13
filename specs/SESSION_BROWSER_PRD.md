# Session browser — product requirements

> Status: **requirements agreed, not started** (2026-11-13). Epic
> `remote-control-cefv`. Decisions B1–B21 were taken with Ruud in a design
> conversation. The macOS spike (`remote-control-cefv.1`) decided O3 as B22;
> the Windows half of the spike is `remote-control-cefv.8`.
> Scope: a web browser inside an agent session in FlightDeck Desktop on macOS
> and Windows, whose requests are made by the machine the session runs on.

## 1. What it is

Inside an agent session you can already open more shells and more agents. A
session can now also hold **browser tabs**, shown in the same area as the
agent and its shells.

The rule the developer can rely on without thinking about it:

> **Whatever address you type, the machine the session runs on fetches it.**

For a session on a worker, `http://localhost:3000` is the dev server on the
worker, `google.com` is fetched from the worker's network (its DNS, its
`/etc/hosts`, its VPN, its IP address), and an internal host only the worker
can reach just works. The page is still **drawn on the Desktop**: scripts run
there and cookies live there. Only the network sits on the worker. For a
session on the Desktop's own machine nothing is special: the browser connects
directly.

There is no hostname rewriting and no per-port mapping. A dev server sees a
request for `localhost:3000` exactly as if a browser on the worker had sent it,
so Vite's allowed hosts, Next.js dev origins, OAuth callbacks, CORS and cookie
domains need no changes.

```
 your machine (FlightDeck Desktop)                    worker
 ┌──────────────────────────────────────────┐          ┌──────────────────────────────┐
 │ session tab                              │          │ FlightDeck worker            │
 │  ├ agent · shell 1 · browser 1           │          │                              │
 │  └ browser view (wry: WKWebView/WebView2)│          │  tunnel route on its port    │
 │      │  every request, any host          │ tailnet  │   token ✓ → connect host:port│
 │      ▼                                   │  TLS     │   (DNS resolved here)        │
 │  SOCKS5 relay 127.0.0.1:<random>  ───────┼──────────┼──▶ localhost:3000            │
 │   (user/password only our views know)    │ one conn │   google.com:443             │
 │                                          │ per conn │   intranet.corp:80           │
 └──────────────────────────────────────────┘          └──────────────────────────────┘
```

## 2. Goals and non-goals

**Goals**

- Look at what the agent is building, in the session, without leaving FlightDeck.
- Make it work the same on a worker as locally, without the developer
  configuring anything.
- Expose nothing new on the tailnet; reuse the worker's pairing and port.
- Phase 2: let the session's agent use the same browser (§6).

**Non-goals**

- Linux Desktop: postponed (nobody uses the Linux client yet, and a wry child
  view only embeds on X11, not Wayland).
- The TUI, FlightDeck Web and the iPhone app: not supported.
- Showing a browser that runs on the worker (pixel streaming).
- A full browser: no bookmarks, history UI, extensions or password manager.

## 3. Decisions

| # | Topic | Decision |
| --- | --- | --- |
| B1 | What | A **browser tab inside an agent session**, next to its agent and shell terminals, drawn in the same area. A session can have several. |
| B2 | Network position | **Every request is made by the machine the session runs on**, for any address. Name resolution happens there too. No hostname rewriting. |
| B3 | Rendering | **On the Desktop**, with the system web view through `wry`: WKWebView on macOS, WebView2 on Windows. No bundled Chromium. |
| B4 | Mechanism | The web view uses a **SOCKS5 proxy with name resolution on the far side**: a relay on the Desktop at `127.0.0.1` (random port, random username and password only our web views know). For each connection, the relay opens one TLS connection to the worker's existing port, with a tunnel token and the target `host:port`. The worker connects and pipes bytes. HTTPS stays end to end: nothing is decrypted in between, and certificates are checked against the real server. |
| B5 | Rejected: Tailscale serve | Exposing the dev server on the tailnet (`tailscale serve`, a worker port per forward) was rejected. It changes the hostname, which breaks Vite allowed hosts, OAuth callbacks, CORS and cookies. It exposes the dev server to the whole tailnet without authentication. And `serve` settings outlive a crashed worker. |
| B6 | Where a tab lives | **In the Desktop window only.** It is not session state on the host, not mirrored to other clients and not persisted by the host. |
| B7 | Platforms | **FlightDeck Desktop on macOS and Windows.** Linux Desktop postponed; TUI, Web and iOS unsupported. |
| B8 | Toolbar | **Ours**, drawn in GPUI: back, forward, reload, the address field with Go, a **"via ‹worker›"** badge for a remote session, and DevTools. |
| B9 | Local session | **No proxy**: the web view connects directly. |
| B10 | Profiles | **One browser profile per session**: cookies, storage and cache are separate, so a login in one worktree's browser does not show up in another. |
| B11 | Port suggestions | The host finds the **TCP ports the session's processes listen on** (agent, extra agents, shells) and the address field offers `http://localhost:<port>`. |
| B12 | Files | `file://` is **blocked** (it would read the Desktop's files, not the worker's). Downloads go to the Desktop's Downloads folder. |
| B13 | Overlays | The web view is a native view drawn above GPUI. While a GPUI overlay (palette, menu, dialog, toast) covers it, the view is **hidden and replaced by a snapshot**. |
| B14 | Security | Only the **window holding the seat** can open tunnels. Background links cannot. Tokens are issued per browser tab and withdrawn when the tab closes, on detach and on takeover. The relay requires its credentials, so other programs on the Desktop cannot use it. A paired Desktop already has a shell on the worker, so this grants nothing new. |
| B15 | Containers | `localhost` is the worker machine. A session running in a container is reached through its published ports (`forward_ports`, published on `127.0.0.1`). |
| B16 | Agent use | **Phase 2.** Agents get browser tools over MCP (§6). |
| B17 | Agent permission | A **per-session switch**, "Let the agent use the browser", **off by default**. `browser_eval` is part of phase 2. |
| B18 | Restoring tabs | The Desktop does **not** reopen a session's browser tabs after it restarts. |
| B19 | Profile lifetime | A session's profile is **deleted when its worktree is abandoned** (the close menu's Abandon worktree). *Built:* a profile is keyed by the worker (or this computer) and the **worktree path**, so closing a session but keeping its worktree keeps its cookies for when it is reopened. A host says, on each `session_removed` frame, whether the worktree went too (`worktree_removed`, additive, checked on the host's own disk); a local window checks its own disk. Only then is the profile deleted, a moment after its web views are gone. |
| B20 | macOS version | The browser needs **macOS 14 or later** (WKWebView proxy support). Older macOS shows "New browser" disabled with the reason. |
| B21 | Self-signed HTTPS | A certificate error page offers **Continue anyway**, remembered for that host and port in that session's profile. *Built (macOS):* `wry`'s navigation delegate handles neither the server-trust challenge nor a failed load, so FlightDeck adds both methods to its class at run time (`browser/tls_macos.rs`, taken from the live delegate; never replacing a method it has). A load that fails on its certificate (NSURLErrorDomain −1200…−1206) shows our page; Continue anyway trusts exactly that `host:port` for that profile, kept in `~/.flightdeck/browser-trust.json` and forgotten with the profile (B19). |
| B22 | Loopback | *From the spike (§4.1).* WKWebView never sends `localhost`, `127.0.0.1` or `[::1]` through a proxy, so on macOS the Desktop **forwards loopback ports itself**: for each loopback port a remote session uses (a port typed in the address field, and every port B11 reports for the session) it listens on `127.0.0.1:<port>` and `[::1]:<port>` and sends each connection through the same tunnel to that port on the worker. The page still asks for `localhost:<port>`. A port already in use on the Mac is shown on our error page by number, never loaded from the Mac. A forward exists only while a browser tab of that session is open. On Windows, WebView2's `<-loopback>` bypass rule may make forwards unnecessary (Windows spike). |
| B23 | Relay credentials | *From the spike.* `wry`'s proxy setting has no credentials, so on macOS the Desktop installs its own Network.framework proxy configuration on the session's data store, with a username and password the relay checks (RFC 1929). Chromium has no SOCKS5 credentials, so Windows needs another check (Windows spike). |
| B24 | Ports on demand | Because we own the address field (B8), a loopback address typed there, or reached by a link or redirect (`wry`'s navigation handler), gets its forward (B22) **before** it loads; a port in use on the Mac is our error page, by number. Requests a page makes by itself (scripts, `fetch`, WebSockets) never pass the address field, which is why B11's ports are forwarded in advance. **Ports are never remapped**: `localhost:3000` is never quietly loaded as another local port, since that changes the page's origin (OAuth callbacks, CORS, cookies, absolute links, Vite's host check). |

## 4. Requirements, phase 1

### 4.1 Spike (`remote-control-cefv.1`, `remote-control-cefv.8`)

`desktop/examples/browser_spike.rs` opens a GPUI window with a `wry` child web
view and runs three servers: one standing for the Desktop's own localhost, a
SOCKS5 relay that logs every target it is asked for, and one standing for the
worker. Run it with `cargo run -p flightdeck-desktop --example browser_spike
[-- wry|match|auth|loopback]`.

**macOS results** (macOS 27, `wry` 0.57.2, 2026-11-13):

| Question | Result |
| --- | --- |
| Child web view in a GPUI window | Works: `build_as_child` on GPUI's `Window`, placed with `with_bounds`; `set_visible(false/true)` works for hiding it under an overlay. |
| Hostnames through the proxy | Yes, **by name**: the relay is asked for `example.com:80` and `only-the-worker-knows.invalid:8080`, so the name is looked up on the far side (B2). |
| WebSockets through the proxy | Yes, by name (hot reload on a non-loopback host works). |
| `localhost`, `127.0.0.1`, `[::1]` | **Always direct**, never the proxy. Adding them as Network.framework match domains does not change that (and makes every other host skip the proxy). Hence B22. |
| Proxy credentials | Work with our own Network.framework configuration (`nw_proxy_config_set_username_and_password`, macOS 14+); the relay saw the username and password on every connection. `wry`'s `with_proxy_config` cannot set them. Hence B23. |
| Proxy fallback | Network.framework never fails over to a direct connection unless told to, so a dead relay is an error page, not a silent direct load. |
| Profile per session with a proxy | Works: `with_data_store_identifier` plus the proxy on that store. The store persists across runs (the page cache came back), and `WebViewExtDarwin::remove_data_store` deletes it (B19). |

**Windows** (`remote-control-cefv.8`, to run on a Windows machine): GPUI does not cross-build
from macOS. To answer: does `--proxy-bypass-list=<-loopback>` send loopback
through the relay (then Windows needs no forwards), how the relay refuses
other local programs without SOCKS5 credentials, and a profile per session
with the proxy. Note that `wry` drops its own proxy flag once browser
arguments are given, so the whole argument line is ours.

### 4.2 Worker (`remote-control-cefv.2`)

*Built (protocol v11; `src/web/tunnel.rs`, `src/web/server/tunnel_route.rs`,
client `src/web/client/tunnel_http.rs`).*

- `open_browser_tunnel` over the socket is answered with a `browser_tunnel`
  frame: a **key**, or a refusal in words. The key is reusable (a page opens
  many connections), bound to the window's credential and connection, and
  ends with `close_browser_tunnel`, when that connection ends (detach, or a
  takeover, which closes the window taken over), or when the worker stops.
  Ending a key cuts every tunnel open on it. At most 32 keys per window.
- `GET /tunnel` on the worker's port (plain or TLS), with `Upgrade:
  flightdeck-tunnel`, the window's cookie, `X-FlightDeck-Tunnel: <key>` and
  `X-FlightDeck-Target: host:port`. The worker resolves and connects **before**
  answering, so a failure is a worded refusal the browser can show:
  `refused` ("Nothing is listening on localhost:3000 on the worker."),
  `unresolved`, `timed_out` (10 s), `bad_target`, `invalid_tunnel`. Then
  `101 Switching Protocols` and bytes both ways. At most 256 tunnels per key.
- The log has one line per tunnel with byte counts, never the target.
- An older worker (below v11): the "New browser" action in a remote session
  explains that the worker needs updating.

### 4.3 Desktop (`remote-control-cefv.3`, `.4`, `.5`)

*Local sessions (`.9`) are built too: the same view with no relay or proxy (B9), New Browser in every Desktop window's palette and sidebar.* *Built for remote sessions on macOS (`.4`): `desktop/src/browser/` (the
view, the address rules, the error page, the macOS proxy) and
`desktop/src/remote/browsers.rs` (which tabs exist, which is on screen). New
Browser is a palette row and a sidebar footer button; each browser is a row
under its session's terminals with a ✕. The window hides the native view
whenever a GPUI overlay, the picker, Mission control, the transfer panel or a
dialog covers it. After a reconnect each tab asks for a new key and its relay
takes it without changing port or password. `flightdeck-desktop
--spike-browser` runs the real view against a stand-in worker. Local
sessions are `remote-control-cefv.9`; Continue anyway for self-signed HTTPS is `remote-control-cefv.10`; Windows
waits for `remote-control-cefv.8`.*

- **Relay:** SOCKS5 on `127.0.0.1`, random port, random credentials (B23).
  Hostnames are passed through and never resolved on the Desktop. Each
  accepted connection opens one tunnel connection. On macOS the proxy is our
  own Network.framework configuration on the session's data store, not
  `wry`'s `with_proxy_config`.
- **Loopback forwards (B22):** on macOS, listeners on `127.0.0.1:<port>` and
  `[::1]:<port>` for each loopback port the session uses, each connection
  sent through the tunnel to that port on the worker. They are bound
  **without `SO_REUSEADDR`** (std's `bind` sets it, and on macOS that lets a
  forward sit in front of a program listening on `0.0.0.0:<port>`), and only
  after the port was found free on the wildcard addresses too (Windows lets
  `127.0.0.1:P` be bound in front of `0.0.0.0:P` regardless). A port in use
  here is refused, never shared.
- *Built:* `src/web/client/relay.rs` (`Relay::start`, `forward`,
  `credentials`, `take_events`). A worker's refusal is the matching SOCKS
  reply code and a `RelayEvent::Refused` with its words, for the error page.
- **Browser tab:** "New browser" in the sidebar footer, the palette and a
  chord. Listed with the session's terminals and shown in the same area. The
  toolbar is B8. The address field accepts a bare `localhost:3000` (adds
  `http://`). It does not search: text that is not an address is refused.
- **Profile per session** (B10).
- The tab title is the page title, falling back to the host.

### 4.4 Port suggestions (`remote-control-cefv.6`)

- The host (local or worker) maps listening TCP sockets to the session's
  process trees and reports them per session (protocol addition for remote).
- The address field's suggestions list them first.

## 5. Failure behaviour

- **Worker unreachable or link lost:** the page shows our own error page
  ("Can't reach the worker"), not the engine's generic one. Reload retries.
- **The target refuses or does not resolve on the worker:** our error page
  says so and names the worker, e.g. "Nothing is listening on localhost:3000 on
  ‹worker›".
- **Taken over by another window:** tunnels close, and the tab shows the
  takeover state like the rest of the window.

## 6. Phase 2: agent browser tools (epic `remote-control-cefv.7`)

The session's agent can use the session's browser, and you watch it do so.

```
agent (in the worktree, on the worker)
  → flightdeck browser-mcp --session <id>   stdio MCP, injected at launch
  → host's control socket
  → the attached Desktop window holding the seat
  → the browser tab, which acts and answers
```

- **Injection:** the same launch-time mechanism as the status hooks
  (`src/agents/setup.rs`), for Claude Code, Codex, Cursor and OpenCode.
- **Tools:** `browser_tabs`, `browser_open`, `browser_navigate`, `browser_back`,
  `browser_reload`, `browser_snapshot` (a compact outline of the page with refs),
  `browser_click` / `browser_type` / `browser_press` by ref, `browser_screenshot`,
  `browser_console` (console messages, errors, failed promises) and `browser_eval`.
- **How the Desktop does them:** reading the page, the snapshot and console use
  scripts we inject. Clicking and typing use events fired from those scripts in
  v1, and real input later where apps reject fired events (macOS system input
  events, WebView2's DevTools Protocol). Screenshots use the engine's own
  capture (`takeSnapshot`, `CapturePreview`).
- **Permission:** the per-session switch (B17), off by default. While an agent
  drives a tab it shows **Agent driving** with **Stop**; any input from you takes over.
- **Limits:** it only works while a Desktop is attached, because the browser
  lives there (B6). With none attached the agent gets a clear error and should
  use its own headless browser. Network detail is limited to what page scripts
  report: the tunnel sees only encrypted bytes.

## 7. Open decisions

| # | Question |
| --- | --- |
| O6 | Windows: forwards or `<-loopback>`, and how the relay checks its caller (`remote-control-cefv.8`). |

O1–O5 were decided as B18–B22.

## 8. Issue map

Children of epic `remote-control-cefv`. "After" lists blocking dependencies.

| Issue | Work | After |
| --- | --- | --- |
| `.1` | Spike on macOS: wry in GPUI, SOCKS5 proxy, loopback, profiles (done) | — |
| `.8` | Spike on Windows: `<-loopback>`, relay access control | — |
| `.2` | Worker tunnel endpoint | — |
| `.3` | Desktop SOCKS5 relay | `.2` |
| `.4` | Browser tabs, toolbar, address field | `.1` |
| `.5` | Profile per session | `.4` |
| `.6` | Port suggestions | `.4` |
| `.7` | Epic, phase 2: agent browser tools | — |
| `.7.1` | `flightdeck browser-mcp` and injection | `.3`, `.4` |
| `.7.2` | Host relays tool calls to the seat window | `.7.1` |
| `.7.3` | Desktop executes the tools | `.7.2` |
| `.7.4` | Per-session switch, Agent driving, Stop | `.7.3` |
| `.7.5` | `browser_eval` | `.7.3` |
