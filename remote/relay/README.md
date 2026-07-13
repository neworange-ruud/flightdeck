# flightdeck-relay

The FlightDeck Remote relay: a New Orange–operated, zero-knowledge broker
between the FlightDeck desktop app and the FlightDeck Remote iOS app. The
desktop keeps a long-lived outbound connection to the relay; phones connect
in; the relay routes ciphertext between them by pairing ID and can never
read the content (see `specs/MOBILE_REMOTE_PRD.md` §9 for the full
architecture).

**This crate is a scaffold.** It has the production shape — HTTP/WebSocket
surface, env-based config, structured logging, graceful shutdown, a
container image, and a CI/deploy pipeline — but no business logic yet.
Routing (matching a phone to its desktop by pairing ID and forwarding
ciphertext) and auth (verifying per-device identity keypairs) are separate,
later tasks. See `src/router.rs`'s doc comment for the seam they plug into.

## Surface

| Endpoint | Purpose |
|---|---|
| `GET /healthz` | Liveness — is the process up at all. |
| `GET /readyz` | Readiness — should traffic be routed here. |
| `GET /version` | Crate version + git SHA of the running build. |
| `GET /ws` | WebSocket upgrade. Today: accepts, answers pings, closes cleanly. No routing. |

## Configuration

Entirely via environment variables (see `src/config.rs`):

| Var | Default | Meaning |
|---|---|---|
| `PORT` | `8080` | TCP port to bind (Azure Container Apps convention). |
| `LOG_FORMAT` | unset (pretty) | `json` for structured logs; anything else (or unset) for human-readable. |
| `RUST_LOG` | `info` | Standard `tracing_subscriber` filter syntax, e.g. `RUST_LOG=info,flightdeck_relay=debug`. |
| `GIT_SHA` | `unknown` | Surfaced on `/version`; set by the deploy environment (see `deploy/containerapp.yaml`). |

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

`tests/integration.rs` spins up the real app (`flightdeck_relay::app`) on an
ephemeral port and drives it like a real client would: `reqwest` for the
HTTP probes, `tokio-tungstenite` as a WebSocket client for `/ws` (ping/pong,
then a clean close).

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

Not automated from a developer machine. `.github/workflows/relay.yml` builds,
tests, and clippy-checks on every push touching `remote/**`, builds the
Docker image, and — only on `main`, and only once the relevant repository
secrets exist — deploys to Azure Container Apps. See
`deploy/README.md` for the one-time `az` setup (resource group, ACR,
Container Apps environment) and `deploy/containerapp.yaml` for the full
desired app configuration (ingress, health probes, scaling).

## What's not here yet

- **Routing** — matching a phone connection to its paired desktop connection
  by pairing ID, and forwarding ciphertext between them. See `src/router.rs`.
- **Auth** — verifying the per-device identity keypair presented at connect
  time.
- **Queued delivery** — holding pending events for a disconnected phone and
  delivering them (deduplicated) on reconnect, per PRD §5.8.

`remote/protocol` (the shared wire-protocol types) is being filled in
concurrently by another task; this crate depends on it by path but
deliberately doesn't import from it yet — that's part of the routing task.
