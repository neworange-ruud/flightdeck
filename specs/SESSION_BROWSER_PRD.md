# Session browser — product requirements

> Status: **requirements agreed, not started** (2026-11-13). Epic
> `remote-control-cefv`. Decisions B1–B17 were taken with Ruud in a design
> conversation; open decisions O1–O5 are listed in §7, and the spike
> (`remote-control-cefv.1`) answers O3 and O4.
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

## 4. Requirements, phase 1

### 4.1 Spike first (`remote-control-cefv.1`)

Before building, prove on macOS and Windows:

1. A `wry` child web view placed in a GPUI window at a given rectangle, kept in
   place on resize, and hidden or replaced by a snapshot while an overlay covers it.
2. A SOCKS5 proxy with credentials on the web view: `proxyConfigurations` on
   macOS 14+, `--proxy-server` on WebView2.
3. **Whether loopback goes through the proxy.** Engines often send `localhost`,
   `127.0.0.1` and `[::1]` directly, which would load the Desktop's own port.
   WebView2 can be told not to (`--proxy-bypass-list=<-loopback>`). macOS is
   the unknown. See O3.
4. Per-session data stores: `WKWebsiteDataStore(forIdentifier:)` and a
   WebView2 profile or user data folder.

### 4.2 Worker (`remote-control-cefv.2`)

- `open_browser_tunnel` over the socket returns a token. Only the window
  holding the seat may ask. The token is withdrawn when the tab closes, on
  detach and on takeover.
- A tunnel route on the worker's port (TLS when the worker has a certificate)
  takes the token and `host:port`, resolves and connects from the worker, and
  pipes both ways until either side closes.
- A cap on concurrent tunnel connections per window. The log records per-tab
  totals, never URLs or hostnames.
- Needs the next protocol version. An older worker: the "New browser" action
  in a remote session explains that the worker needs updating.

### 4.3 Desktop (`remote-control-cefv.3`, `.4`, `.5`)

- **Relay:** SOCKS5 on `127.0.0.1`, random port, random credentials. Hostnames
  are passed through and never resolved on the Desktop. Each accepted
  connection opens one tunnel connection.
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
| O1 | Does the Desktop reopen a session's browser tabs (their URLs) after it restarts? |
| O2 | When is a session's profile deleted: when the session closes, when its worktree is removed, or never? |
| O3 | If WKWebView sends loopback past the proxy: fall back to a local port forward of the requested port (fails visibly when the port is taken on the Desktop)? Decided after the spike. |
| O4 | Proxy support in WKWebView needs macOS 14. Is that the browser's minimum, with older macOS showing it unavailable? |
| O5 | A dev server with a self-signed HTTPS certificate: offer "Continue anyway" per session? |

## 8. Issue map

Children of epic `remote-control-cefv`. "After" lists blocking dependencies.

| Issue | Work | After |
| --- | --- | --- |
| `.1` | Spike: wry in GPUI, SOCKS5 proxy, loopback, profiles | — |
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
