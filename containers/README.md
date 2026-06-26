# Container support for FlightDeck (SPECS §31)

FlightDeck can run each agent inside an isolated, rootless **Podman** container
instead of directly on the host. The agent's git worktree is bind-mounted at
`/workspace`; the host keeps owning the worktree and all git operations.

This is a **project-wide toggle** — when enabled, all agents run in containers.

## 1. Enable containers

In `.flightdeck/config.toml`:

```toml
[execution]
enabled = true
runtime = "podman"

# By default FlightDeck builds a self-contained image from a trusted public
# base (docker.io/library/node) and installs the agent CLI — no pre-built base
# needed. Optional per-project customization layered on top:
# packages = ["postgresql-client", "jq"]
# setup_script = ".flightdeck/agent-setup.sh"
# Pin/replace the base image (must be Debian-family, carry the agent + `agent` user):
# base_image = "localhost/flightdeck-claude-base:latest"
# Or bring your own Containerfile entirely:
# containerfile = "containers/agent.Containerfile"

# Optional: publish dev-server ports to 127.0.0.1
# forward_ports = [3000]

[execution.limits]
cpu = "4"
memory = "8g"
pids = 512

# Credentials: mount host creds read-only, and/or inject an allowlisted env var.
[execution.auth]
env_allow = ["ANTHROPIC_API_KEY"]
# [[execution.auth.mounts]]
# host_path = "~/.claude"
# container_path = "/home/agent/.claude"
# writable = false
```

## 2. Build the image and check readiness

```bash
flightdeck image build claude   # pulls the trusted base, installs the agent CLI
flightdeck doctor               # verifies podman + images are ready
```

The reference `Containerfile.*` in this directory are **optional** — only needed
if you want to pre-build and pin a base via `execution.base_image`.

## 3. Run

Launch FlightDeck as usual. New agent tabs run in a container named
`flightdeck-<tab-id>`; child shells (`Ctrl-t`) `podman exec` into the same
container. Containers persist across FlightDeck restarts — a still-running one
is reattached; an exited one shows as "session lost" for a manual restart.

## Security model (non-disableable guardrails)

Every container launch is checked by `src/runtime/guards.rs` and rejected if it
would: run `--privileged`, mount a docker/podman socket, use `--env-host`, mount
the home directory, or publish a port on anything but `127.0.0.1`. Containers run
with `--cap-drop all` and `--security-opt no-new-privileges`.

> Network egress is **not** restricted in v1 (full outbound). An allowlist proxy
> is a planned follow-up.
