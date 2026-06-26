# Container support for FlightDeck (SPECS §31)

FlightDeck can run each agent inside an isolated, rootless **Podman** container
instead of directly on the host. The agent's git worktree is bind-mounted at
`/workspace`; the host keeps owning the worktree and all git operations.

This is a **project-wide toggle** — when enabled, all agents run in containers.

## 1. Build a base image

FlightDeck's per-project image builds *on top of* a base image that carries the
agent CLI and a non-root, UID-mappable `agent` user. Build the base once:

```bash
podman build -t flightdeck-claude-base:latest -f containers/Containerfile.claude containers
# or codex / opencode
```

## 2. Enable containers

In `.flightdeck/config.toml`:

```toml
[execution]
enabled = true
runtime = "podman"

# Optional per-project customization layered on the base image:
# packages = ["postgresql-client", "jq"]
# setup_script = ".flightdeck/agent-setup.sh"
# Or bring your own Containerfile (must FROM a flightdeck base):
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

## 3. Build the project image and check readiness

```bash
flightdeck image build claude   # builds base + customization → project image
flightdeck doctor               # verifies podman + images are ready
```

## 4. Run

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
