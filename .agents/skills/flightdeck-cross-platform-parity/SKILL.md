---
name: flightdeck-cross-platform-parity
description: Use when writing or reviewing FlightDeck code with OS-specific behavior — keybindings, clipboard, paths, notifications, process/signal handling, PTY, or containers. FlightDeck must behave identically on macOS, Linux, and Windows.
---

# FlightDeck cross-platform parity

Every change must work on **macOS, Linux, and Windows**. A large share of this
repo's bugs have been platform-specific (clipboard, path separators,
`USERPROFILE`, `notify-send`, `Shift+Esc`, signal/process-group kill vs
`fs::rename`). A change that only handles one OS is unfinished.

## Symmetric per-OS constants, not a `windows` boolean

Branch on the three constants in `src/tui/platform.rs`, checked per OS:

```rust
platform::IS_WINDOWS   // not `let windows = ...`
platform::IS_LINUX
platform::IS_MACOS
// compose them: LEAVE_FOCUS_USES_SHIFT = IS_WINDOWS || IS_LINUX
```

A yes/no `windows` flag was rejected in review in favor of three symmetric
variables. Adding a fourth OS should mean adding a constant, not inverting a
boolean.

## Exhaustive `cfg` with an explicit no-op fallback

When gating with `#[cfg(...)]`, cover every target and give the remainder a
defined no-op so other hosts stay silent-but-compiling — no dead-code or
unreachable warnings:

```rust
#[cfg(target_os = "macos")]  fn post(..) { post_macos(..) }
#[cfg(target_os = "linux")]  fn post(..) { post_linux(..) }
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn post(..) { /* no-op: platform has no backend */ }
```

## Keep OS-specific code out of core logic

Isolate platform branches at the edges (`notify`, `terminal`, `runtime`, `fs`);
the app-core state machine and git logic stay platform-agnostic. Prefer
compile-time `cfg`/constants for behavior fixed per target; use runtime checks
only when one binary must adapt at run time.

## External tools: argv, never a shell; never block the render loop

Spawn helpers (`notify-send`, `wl-copy`/`xclip`, `podman`) via argv — no shell
string interpolation (no injection). Run best-effort side effects on a detached
thread so a missing binary fails silently and never stalls rendering.

These helpers usually sit behind a trait (`Notifier`, `ContainerRuntime`), so a
new one is also an architecture-seam change — consult
flightdeck-architecture-seams alongside this skill.

## Verify across platforms

Dispatch a review subagent with the explicit macOS/Linux/Windows mandate and
enough context to assess the change on all three. After pushing, confirm the
Windows CI job is green (see shipping-flightdeck-changes).
