#!/usr/bin/env bash
# Start (or restart) the tutorial worker container and print a pairing code.
#
#   tutorials/worker/up.sh            build binary + image, start fresh
#   SKIP_BUILD=1 tutorials/worker/up.sh   reuse the last binary
#
# The container is recreated on every run so each recording starts from the
# same state. Only ~/.claude survives (a named volume), so the agent login
# is done once.
#
# BIND is the host address the worker port is published on. The default is
# the host side of Tart's NAT bridge, so only the tutorial VMs can reach it;
# it exists only while a Tart VM is running, so start the VM first.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NAME="${WORKER_NAME:-fd-tutorial-worker}"
BIND="${BIND:-192.168.64.1}"
PORT="${PORT:-7420}"

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  "$ROOT/tutorials/worker/build-binary.sh"
fi
CONTEXT="$ROOT/target/tutorials/worker-context"
rm -rf "$CONTEXT" && mkdir -p "$CONTEXT"
cp "$ROOT/target/tutorials/linux/flightdeck" "$CONTEXT/"
cp -R "$ROOT/tutorials/projects" "$CONTEXT/projects"
podman build -t fd-tutorial-worker -f "$ROOT/tutorials/worker/Containerfile" "$CONTEXT"

podman rm -f "$NAME" >/dev/null 2>&1 || true
podman run -d --init --name "$NAME" --hostname devbox \
  -p "$BIND:$PORT:7420" \
  -v fd-tutorial-claude:/home/dev/.claude \
  fd-tutorial-worker >/dev/null

for _ in $(seq 1 50); do
  if podman exec "$NAME" flightdeck worker status >/dev/null 2>&1; then break; fi
  sleep 0.2
done
# Offer only the agent this box has: the New Agent form then lists just Claude Code.
for agent in codex cursor opencode; do
  podman exec "$NAME" flightdeck worker agents disable "$agent" >/dev/null
done
podman exec "$NAME" flightdeck worker status
echo
echo "Worker reachable from the VM at $BIND:$PORT"
podman exec "$NAME" flightdeck worker pair
