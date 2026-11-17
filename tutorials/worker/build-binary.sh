#!/usr/bin/env bash
# Build a Linux `flightdeck` binary for the tutorial worker container.
#
# A DEBUG build on purpose: only debug builds honour
# FLIGHTDECK_WORKER_TEST_LISTEN, which lets the worker listen on a fixed
# address instead of this machine's Tailscale addresses (a release worker
# listens on the tailnet only, and a tailnet does not belong in a video).
#
# The source is mounted read-only into a rust container; the cargo registry
# and target dir live in named Podman volumes, so a rebuild is incremental.
# Output: target/tutorials/linux/flightdeck (arch of the Podman machine).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="$ROOT/target/tutorials/linux"
IMAGE="${RUST_IMAGE:-docker.io/library/rust:1-bookworm}"
mkdir -p "$OUT"

# FlightDeck Web (the browser client) is embedded from webui/dist at build
# time; without it the worker serves a "webui was not built" page.
(cd "$ROOT/webui" && npm ci --silent && npm run build --silent >/dev/null)

podman run --rm \
  -v "$ROOT":/src:ro \
  -v fd-tutorial-cargo-registry:/usr/local/cargo/registry \
  -v fd-tutorial-target:/target \
  -v "$OUT":/out \
  -e CARGO_TARGET_DIR=/target \
  -w /src \
  "$IMAGE" \
  bash -c 'cargo build --locked -p flightdeck --bin flightdeck && cp /target/debug/flightdeck /out/flightdeck'

file "$OUT/flightdeck" 2>/dev/null || true
echo "built: $OUT/flightdeck"
