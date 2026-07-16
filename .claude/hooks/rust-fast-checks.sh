#!/usr/bin/env bash
# Stop hook — fast Rust checks (fmt + clippy) for FlightDeck.
#
# Runs only when the working tree has changed .rs files, so ordinary stops stay
# cheap. On failure it exits 2, which feeds the message on stderr back to the
# agent so it fixes the problem before the turn ends. The full `cargo test` run
# is deliberately NOT here (too slow per stop) — it stays in the
# shipping-flightdeck-changes definition of done.
#
# Disable by removing the Stop hook from .claude/settings.json.
set -uo pipefail

cd "${CLAUDE_PROJECT_DIR:-.}" || exit 0

# Skip unless Rust sources changed in the working tree.
if ! git status --porcelain 2>/dev/null | grep -qE '\.rs$'; then
  exit 0
fi

if ! out=$(cargo fmt --check 2>&1); then
  printf 'Stop hook: `cargo fmt --check` failed — run `cargo fmt`.\n\n%s\n' "$out" >&2
  exit 2
fi

if ! out=$(cargo clippy --all-targets --locked -- -D warnings 2>&1); then
  printf 'Stop hook: `cargo clippy` reported warnings/errors — fix them.\n\n%s\n' "$out" >&2
  exit 2
fi

exit 0
