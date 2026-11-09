# Contributing to FlightDeck

User documentation lives on the docs site (source in [`web/docs/`](web/docs)).
This file is for working on FlightDeck itself: how the code is laid out, how to
build and test it, how releases are cut, and the manual smoke test.

Before you finish a change, run the ship gate described in
[`.agents/skills/shipping-flightdeck-changes`](.agents/skills/shipping-flightdeck-changes)
and add an entry to `CHANGELOG.md` (CLI/TUI and shared core) and/or
`desktop/CHANGELOG.md` (desktop app).

## Architecture

Business logic is separated from the TUI and fully testable. Git, the
filesystem, and PTYs sit behind traits (`src/contracts/traits.rs`) so every logic
module is unit-tested against fakes (`src/testing/`). The TUI dispatches
`Command`s into the headless app core, which calls services — the TUI never runs
git/fs/pty itself.

```text
src/
  contracts/   shared types, traits, errors, trivial real impls
  testing/     FakeGit / FakeFs / FakePty / FakeClock
  config/      load/serialize config.toml, defaults, first-run init
  fs/          relative/absolute paths, append-only .gitignore updater
  git/         real GitExecutor + branch/worktree/status/remote workflow logic
  agents/      registry, PATH validation, lifecycle-status integrations
  persistence/ state.json load/save + worktree recovery
  terminal/    portable-pty backend + session model (primary + child shells)
  app/         headless state, commands, dispatch, input modes
  tui/         ratatui layout, render, key mapping, command palette
  lib.rs       run(): startup → recovery → event loop → clean teardown
tests/
  integration/ real temp-git-repo workflow tests
  guards.rs    SPECS §2 (naming) and §5 (no history rewriting) guards
```

## Development

Requires a Rust toolchain (stable) and `git`.

```bash
cargo build                              # debug build
cargo build --release                    # release build
cargo test                               # unit + integration + guard tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo run                                # run inside a git repo
```

### Build the desktop app

```sh
cargo run -p flightdeck-desktop            # from inside a git repository
cargo build -p flightdeck-desktop --release
```

The desktop app is a separate package in the workspace, so a plain `cargo build`
at the root still builds only the terminal UI. Started outside a git repository,
the window shows a project picker with your recent projects. `--isolated` (`-I`)
works as in the terminal UI.

- **macOS.** Xcode or the Command Line Tools. The default `runtime-shaders`
  feature compiles GPUI's Metal shaders when the app starts, so you do not need
  the separate Metal Toolchain download that Xcode 26 and later require.
- **Linux**. A C toolchain and the windowing libraries. On
  Debian or Ubuntu:
  `sudo apt install build-essential pkg-config cmake clang libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev libxcb1-dev libfontconfig-dev libfreetype-dev`.
  At run time it needs a Vulkan loader and driver (`libvulkan1`,
  `mesa-vulkan-drivers`).
- **Windows**. The MSVC toolchain with the Windows 10/11 SDK
  installed; the build compiles shaders with `fxc.exe` from the SDK.

The design and the decisions behind the desktop app are in
[`specs/DESKTOP_UI.md`](specs/DESKTOP_UI.md).

### Cross-building for Windows

The app compiles and runs on Windows. You can build a Windows `.exe` from
macOS/Linux without a Windows machine — cross-compilation is
CPU-architecture-independent, so it works on Apple Silicon too:

```bash
scripts/build-windows                    # release .exe via cargo-xwin in Docker
scripts/build-windows debug              # debug build
```

This uses [`cargo-xwin`](https://github.com/rust-cross/cargo-xwin) in Docker to
download the MSVC CRT/SDK import libraries and link with LLVM `lld` — no MSVC
install required. The binary lands at
`target/x86_64-pc-windows-msvc/release/flightdeck.exe`. The container ships the
clang-based C cross-toolchain the dependency graph needs (the self-update path
pulls in `aws-lc-sys`, a C crypto library), so a bare `cargo check --target
x86_64-pc-windows-msvc` on the host is **not** sufficient — use the script (or
CI) for any Windows compile check.

Building is one thing; **running** a Windows binary needs Windows. The
`windows-latest` CI job (see `.github/workflows/ci.yml`) compiles, lints, and
runs the test suite on real Windows on every push — that is the canonical check.
To exercise the TUI interactively on an Apple Silicon Mac, use a Windows 11 on
ARM VM (its built-in x64 emulation runs the x86_64 binary); Wine's console
emulation is not reliable for a full-screen PTY app.

## Release

The CLI/TUI and the desktop app are released separately. The CLI ships on
`v<x.y.z>` tags (below); the desktop app ships on `desktop-v<x.y.z>` tags with
`scripts/release-desktop`, and its CI, packaging and signing are described in
[`desktop/PACKAGING.md`](desktop/PACKAGING.md). Release notes come from
`CHANGELOG.md` and `desktop/CHANGELOG.md` respectively.

### CLI / terminal UI

Releases are built by `cargo-dist` in GitHub Actions when a version tag is
pushed. To publish a release, install `dist` once if needed:

```bash
cargo install cargo-dist --version 0.32.0 --locked
```

Then run one command from a clean working tree:

```bash
./scripts/release 0.2.0
```

The script moves the current `## [Unreleased]` notes into a new versioned
`CHANGELOG.md` entry dated for the release, resets `Unreleased` back to the
empty template, updates `Cargo.toml`, refreshes `Cargo.lock`, runs formatting,
Clippy, tests, validates the cargo-dist plan, commits `Release v0.2.0`, tags it,
and pushes the branch and tag. GitHub Actions then creates the GitHub Release,
uploads the installer/artifacts/checksums, and publishes the Homebrew formula to
`neworange-ruud/homebrew-tap`.

Repository setup required for Homebrew publishing:

- Create the `neworange-ruud/homebrew-tap` repo if it does not exist.
- Add a `HOMEBREW_TAP_TOKEN` repository secret with permission to push to that
  tap.
- Ensure Actions has read/write workflow permissions for release creation.

#### macOS code signing

Release binaries are signed in CI with an Apple Developer ID Application
certificate (`macos-sign = true` in `dist-workspace.toml`) and built with the
**hardened runtime** enabled. The hardened runtime is turned on via
`.github/workflows/build-setup.steps`, which exports `CODESIGN_OPTIONS=runtime`
before `dist build` (cargo-dist has no dedicated config key for it). That file
is injected into the build job through the `github-build-setup` key, so
`dist generate --check` stays green — edit `build-setup.steps` rather than
`release.yml` directly, then re-run `dist generate`.

Required repository secrets (a missing one silently disables signing):

- `CODESIGN_CERTIFICATE` — base64-encoded `.p12` (Developer ID Application cert
  plus private key).
- `CODESIGN_CERTIFICATE_PASSWORD` — passphrase for that `.p12`.
- `CODESIGN_IDENTITY` — the identity string, e.g.
  `Developer ID Application: <name> (<TEAMID>)`.

Releases are **signed but not notarized** (cargo-dist 0.32 does not notarize).
This is fine for the Homebrew and `curl | sh` install paths, which do not apply
a Gatekeeper quarantine. Artifacts downloaded through a web browser would be
quarantined and rejected by Gatekeeper until notarized — add a
`notarytool` + `stapler` step if browser distribution is ever needed.

To verify a built binary: `codesign -dv --verbose=4 <path>` should show the
Developer ID authority and `flags=0x10000(runtime)`.

## Manual smoke test (human, requires a real terminal)

Automated tests cannot drive a real attached terminal/PTY end-to-end. After
changes, run this checklist by hand from inside a scratch Git repo:

1. `cargo run` inside a git repo → FlightDeck starts; `.flightdeck/` is created
   and `.gitignore` gains the two entries (notice shown). A branded logo header
   and divider span the top of the screen.
2. **New tab** (`Ctrl-n`) → pick an agent from the menu (e.g. Claude Code) →
   enter a name → the `flightdeck/<slug>` branch + worktree are created and the
   chosen agent launches in the primary terminal.
3. **Missing agent**: edit `config.toml` to a bogus `command`, create a tab →
   creation fails with a clear message and **no** branch/worktree is created.
4. **Child terminal** (`Ctrl-t`) → a shell opens in the same worktree; switch
   with `Alt-←/→` (or click its tab); close with `Ctrl-w`. The agent's and each
   shell's live output renders in the main pane.
5. **Git info bar**: the line above the status bar shows the selected tab's
   branch, change counts, ahead/behind, and base — and stays correct whether the
   agent or a shell tab is focused.
6. **Git status** (palette → *Show Git Status*) → branch, base, drift, dirty,
   ahead/behind, worktree path.
7. **Push** (`Ctrl-p`) → with uncommitted changes you are warned; after a commit,
   confirm push → a GitHub PR compare URL is shown (if origin is GitHub).
8. **Manual status** (`Ctrl-s`) → set/clear; the process state stays visible.
9. **Abandon worktree** (palette → *Abandon Worktree*) → a clean worktree is
   removed at once; with uncommitted changes you are asked to confirm discarding
   them before it is force-removed.
10. **Close tab** (`Ctrl-k`) → the option menu defaults to *Send Ctrl-C to
    primary*; it never auto-escalates to force-kill.
11. **Quit**: `Ctrl-q`, or open the palette (`Ctrl-g`) and choose *Quit* — both
    exit cleanly.
12. **Recovery / resume**: quit (`Ctrl-q`), relaunch → tabs are reconstructed
    from disk and their agents are restarted automatically when their worktree
    still exists (you can also restart any tab manually with `Ctrl-r`).
13. **No orphans**: after quitting, confirm no agent/shell child processes are
    left running (e.g. `pgrep -fl opencode`).
