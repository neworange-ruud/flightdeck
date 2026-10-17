# flightdeck-relay

The FlightDeck Remote relay: a New Orange–operated, zero-knowledge broker
between the FlightDeck desktop app and the FlightDeck Remote iOS app. The
desktop keeps a long-lived outbound connection to the relay; phones connect
in; the relay routes ciphertext between them by pairing ID and can never
read the content (see `specs/MOBILE_REMOTE_PRD.md` §9 for the full
architecture).

It implements the wire protocol in `specs/REMOTE_PROTOCOL.md`: version
negotiation, per-device ECDSA P-256 challenge-response auth, pairing bootstrap,
zero-knowledge envelope routing with a server-side queue (gapless seq, resume,
cumulative acks), push-token registration, and desktop-requested encrypted
pushes. One binary runs in one of two **relay modes** (below).

## Surface

| Endpoint | Purpose |
|---|---|
| `GET /healthz` | Liveness — is the process up at all. |
| `GET /readyz` | Readiness — should traffic be routed here. |
| `GET /version` | Crate version, git SHA and relay mode of the running build: `{"version","git_sha","relay_mode"}`. |
| `GET /ws` | WebSocket upgrade: the relay protocol (`specs/REMOTE_PROTOCOL.md` §5–§6), including the desktop's `push_notify`. |
| `POST /v1/push` | HTTPS twin of `push_notify` for a desktop without a live WebSocket (spec §5.5.3): a signed `PushRequest`; `202` on accept, JSON `PushErrorBody` otherwise. |

### Rollout order

Deploy a v4 relay **before** updating desktops, and desktops before the iPhone
app. A v4 relay sends no silent wake pushes any more, so a v3 desktop gets no
pushes from it; a v3 relay has neither `push_notify` nor `POST /v1/push` (a v4
desktop's push gets 404 and is dropped); and the v4 iPhone app no longer acts on
wake pushes, so it should arrive once pushes come as v4 alerts.

### Pushes

The relay sends a push only when a **desktop asks for one** (`push_notify` on the
WebSocket, or `POST /v1/push`): one visible APNs alert with generic per-class
text and the E2E-sealed payload in `fd_push`, which the phone's Notification
Service Extension decrypts. Queued envelopes never trigger a push. Both paths
share `src/push.rs` (membership, limits, per-desktop rate limit, token lookup);
`src/apns.rs` builds and sends the request. One log line per push attempt
records the pairing, event id, class and outcome — never content or tokens.

## Relay modes

Set with `RELAY_MODE` (spec §5.9). The relay announces its mode to every client
in `hello_ok.relay_mode`, logs it at startup, and reports it on `GET /version`.

| | `full` (default) | `push_only` |
|---|---|---|
| What it is | The routing relay: phone and Mac talk **through** it. | A push and pairing service only: phone and Mac talk **directly**. |
| Hello / auth, pairing offer + claim | ✓ | ✓ |
| Machine name, revoke, presence, ping | ✓ | ✓ |
| Push-token register / unregister | ✓ | ✓ |
| `push_notify` (WebSocket) + `POST /v1/push` | ✓ | ✓ |
| `envelope`, `resume`, `ack` (the data plane) | ✓ routed + queued | refused: `error { code: relay_mode_push_only }` |
| Envelope queues / seq tracking | per pairing, bounded by `QUEUE_MAX_PER_PAIRING` | never created (queue tables stay empty) |

**`full`** is what a self-hoster gets by default and needs nothing beyond the
relay: both apps reach it over the internet and it carries their (end-to-end
encrypted) traffic, queueing for whichever side is offline.

**`push_only`** carries no app traffic. Choose it only when **every** pairing is
a *direct-mode* pairing (spec §5.10):

- The phone must be able to reach the Mac directly — in practice **Tailscale on
  both** the Mac and the phone, on the same tailnet (any other private route
  that lets the phone open `ws://<mac>:<port>` works too). Away from that
  network, the phone cannot reach the Mac at all; the relay will not fill in.
- The Mac must run a FlightDeck desktop with the **direct link enabled**, so it
  serves the phone protocol itself and puts its Tailscale address in the
  pairing offer.
- Pair (or re-pair) **after** enabling the direct link. Pairings made before it,
  or against a `full` relay without it, are relay-mode pairings: on a push-only
  relay they authenticate and get pushes but their envelopes are refused, so
  the phone shows nothing. A push-only relay will not *create* such a pairing:
  a claim that would be relay mode (the offer had no direct endpoint, or the
  phone app is older than v4) is refused with `pairing_claim_rejected` and a
  message saying what to do, sent to the phone and to the waiting desktop.

Pairing itself still goes through the relay in both modes — it is where the two
devices meet before either knows the other's address, and where the relay
learns the device keys it needs to authenticate pushes and token
registrations. The relay keeps storing device keys, pairings and push tokens
(use `FLIGHTDECK_RELAY_STORE=sqlite:…` so they survive restarts, exactly as in
`full`), APNs must be configured for pushes to arrive, and
`FLIGHTDECK_RELAY_PASSWORD` gates both the WebSocket and `POST /v1/push`.

```bash
RELAY_MODE=push_only cargo run -p flightdeck-relay   # or: docker run -e RELAY_MODE=push_only …
curl localhost:8080/version                          # {"version":"…","git_sha":"…","relay_mode":"push_only"}
```

An unrecognized `RELAY_MODE` stops the relay at startup with
`configuration error: invalid RELAY_MODE: …` (exit status 2), rather than
quietly running as a router. Unset or empty means `full`; the value is trimmed
and case-insensitive.

## Configuration

Entirely via environment variables (see `src/config.rs`):

| Var | Default | Meaning |
|---|---|---|
| `PORT` | `8080` | TCP port to bind (Azure Container Apps convention). |
| `LOG_FORMAT` | unset (pretty) | `json` for structured logs; anything else (or unset) for human-readable. |
| `RUST_LOG` | `info` | Standard `tracing_subscriber` filter syntax, e.g. `RUST_LOG=info,flightdeck_relay=debug`. |
| `GIT_SHA` | `unknown` | Surfaced on `/version`; set by the deploy environment (see `deploy/containerapp.yaml`). |
| `RELAY_MODE` | `full` | `full` or `push_only` — see [Relay modes](#relay-modes). Anything else refuses to start. |
| `FLIGHTDECK_RELAY_STORE` | `memory` | `sqlite:<path>` persists device keys, pairings, claim tokens, push tokens (and, in `full` mode, the envelope queues) across restarts. |
| `FLIGHTDECK_RELAY_PASSWORD` | unset (open) | Shared admission password for `hello.relay_password` and the `x-flightdeck-relay-password` header on `POST /v1/push`. |
| `APNS_TEAM_ID`, `APNS_KEY_ID`, `APNS_TOPIC`, `APNS_AUTH_KEY_PEM` (or `APNS_AUTH_KEY_PATH`), `APNS_ENVIRONMENT` | unset (push off) | APNs credentials; push also needs an image built with `--features apns-live`. See `deploy/README.md`. |
| `PUSH_RATE_LIMIT_PER_MINUTE` | `60` | Accepted pushes per desktop per minute (WebSocket + HTTPS together); over it → `rate_limited` / 429. |
| `QUEUE_MAX_PER_PAIRING` | `1000` | Un-acked envelopes held per pairing direction before drop-oldest (`full` mode only). |

The process shuts down gracefully on SIGTERM (what Container Apps sends on
scale-down/redeploy) or Ctrl-C, draining in-flight requests/connections
first.

## Local run

```bash
cd remote
cargo run -p flightdeck-relay
# in another shell:
curl localhost:8080/healthz
curl localhost:8080/version
```

## Tests

```bash
cd remote
cargo test -p flightdeck-relay
```

The integration tests spin up the real app on an ephemeral port and drive it
like a real client would: `reqwest` for HTTP, `tokio-tungstenite` as a
WebSocket client for `/ws`. `tests/relay_flow.rs` covers the `full` data plane,
`tests/push_notify.rs` the push paths, and `tests/push_only.rs` the push-only
mode (control plane served, data plane refused, queues never touched).

Quality gates, run from `remote/`:

```bash
cargo test -p flightdeck-relay
cargo clippy -p flightdeck-relay --all-targets -- -D warnings
cargo fmt -p flightdeck-relay -- --check
```

## Docker

The build context is the `remote/` workspace root, **not** `remote/relay/`,
because this crate path-depends on `../protocol`:

```bash
cd remote
docker build -f relay/Dockerfile -t flightdeck-relay .
docker run --rm -p 8080:8080 flightdeck-relay
curl localhost:8080/healthz
```

The image is a multi-stage build: a `rust:1-bookworm` builder stage, and a
`gcr.io/distroless/cc-debian12:nonroot` runtime stage (no shell, no package
manager, runs as uid `65532`). ~38 MB final image.

## Deploying

Live on Azure Container Apps. The concrete URL and resource names aren't in this
public repo — retrieve the ingress URL with
`az containerapp show … --query properties.configuration.ingress.fqdn` (resource
names are in the repo's Actions variables).

CI (`.github/workflows/relay.yml`) formats, clippy-checks, tests, and
build-checks the image on every push/PR touching `remote/**` — it does not
deploy. **Deployment runs from `.github/workflows/relay-deploy.yml` when a
GitHub Release is published** (or via manual dispatch): it builds + pushes the
image and runs `az containerapp update`, authenticating via GitHub OIDC (no
stored Azure secret). See `deploy/README.md` for the full runbook — live
resource names, the reproducible `az` setup, and cost — and
`deploy/containerapp.yaml` for the desired app configuration.

## Not addressed yet

- **Horizontal scale** — routing and pairing notifications are per-process and
  the SQLite store is single-writer, so the relay runs as one replica in both
  modes (see `deploy/README.md`).
