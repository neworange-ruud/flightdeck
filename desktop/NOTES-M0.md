# flightdeck-desktop: M0 notes (GPUI scaffold)

Written for beads issue `remote-control-bmej.1.1`. This records how GPUI is consumed,
what was rejected and why, how to upgrade, the licence scan, per-OS build
prerequisites, and what this milestone could not check.

Verified on 2026-09-29, macOS (Darwin 27, Apple M2 Pro, Xcode 27.0), rustc 1.96.0.

## The pin

| Crate (as named in `desktop/Cargo.toml`) | crates.io package | Version |
| --- | --- | --- |
| `gpui` | `gpui-pre` | `=0.3.7` |
| `gpui_platform` | `gpui-pre-platform` | `=0.3.7`, features `font-kit`, `wayland`, `x11` (+ `runtime_shaders` via our default feature) |
| `gpui-component` | `gpui-component` | `0.7.0` |

All three come from crates.io. The build uses no git dependencies.

**Why this combination:** `gpui-component` 0.7.0 (released 2026-09-28) depends on
`gpui-pre = "=0.3.7"` with an exact pin. Its version therefore sets the GPUI version,
and `gpui-pre` 0.3.7 is the only GPUI that works with the current component library.
`gpui-pre` is the Zed team's own crates.io snapshot of `gpui`: it comes from the same
repo (`zed-industries/zed`) under the same Apache-2.0 licence and is published about
weekly (0.3.2 through 0.3.7 came out in September 2026). Since Zed's platform split,
`gpui` has no windowing backend of its own. `gpui-pre-platform` chooses the backend
for each OS and exposes `application()`.

We declare `gpui-pre` directly, renamed to `gpui`, even though `gpui-component`
already brings it in. That lets our own code say `use gpui::…` like every GPUI example
does. The `=` pin says outright what the component pin forces anyway.

### Rejected candidates

- **crates.io `gpui` 0.2.2.** It was last published 2025-10-22, which is before the
  platform split and the whole 0.3 API line. No current `gpui-component` builds
  against it. Using it would mean pinning `gpui-component` to a year-old release.
- **`gpui-ce` (community fork).** Its version history is confusing: 0.3.3 came out in
  December 2025, then 0.2.2 in August 2026 became `max_version`. It still uses the old
  `cocoa`/`metal` bindings, and `gpui-component` does not depend on it. We would have
  to fork or patch the component library to use it. That trades Zed's upstream for a
  smaller maintainer pool and gains nothing.
- **A pinned git rev of `zed-industries/zed`.** This works in principle, but a git
  dependency drags the entire Zed monorepo checkout into every clean build and CI
  cache. It also can't be published, and `gpui-component` from crates.io would pull
  a *second*, crates.io copy of gpui next to it. Two copies of gpui means two
  incompatible `App` types. Only consider this if we ever need an unreleased upstream
  fix, and then use `[patch.crates-io]` so there is still a single gpui.
- **`gpui-kit` (the umbrella crate that re-exports gpui + gpui-base +
  gpui-component).** This one is viable, but it hides the GPUI pin behind a
  re-export. We want the pin visible in our own manifest.

## Upgrade policy

1. **Move all three together, and let `gpui-component` lead.** Upgrade only to a
   `gpui-component` release, then set `gpui` and `gpui_platform` to the exact
   `gpui-pre` version it pins. Never bump `gpui-pre` on its own: cargo would refuse,
   or it would end up with two GPUIs.
2. **Upgrade on purpose, not on a timer.** `gpui-pre` is a pre-1.0 weekly snapshot
   and its API breaks between minors. Upgrade when we need a fix or a component,
   and at most about once per milestone. Each upgrade is its own PR with nothing else
   in it.
3. **Recheck on every upgrade:** `cargo build -p flightdeck-desktop --locked`,
   clippy, tests, and a real launch. Rerun the licence scan below, because Zed's
   graph can gain crates. Rerun the root-package tree diff as well (see "Lock impact").
4. `cargo update` in the root must not upgrade GPUI by accident. The `=` pins
   guarantee that.

## Workspace / lock impact

The root `Cargo.toml` is now a workspace, `members = [".", "desktop"]` with
`default-members = ["."]`. Every root command (`cargo build`, `cargo test`, `cargo
clippy --all-targets`, cargo-dist) still covers only the `flightdeck` TUI package.
`remote/` is excluded and keeps its own workspace and lock.

- **One cargo quirk forced a `[workspace.package]` table in the root.**
  `remote/protocol` inherits `edition`/`license`/`repository` from `remote/Cargo.toml`.
  Once the root became a workspace, cargo resolved that inheritance against the
  already-loaded outer workspace and failed with "`workspace.package.edition` was not
  defined". `exclude` does not fix this. The root now mirrors `remote/`'s three values
  exactly, and a comment in `Cargo.toml` explains why. If `remote/Cargo.toml` ever
  changes those values, the two must be kept in sync.
- **What changed for the root package.** I diffed the `cargo tree -p flightdeck -e
  normal,build` output (packages and enabled features) before and after, for
  `aarch64-apple-darwin`, `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-gnu`. The
  package counts are identical (367 / 257 / 362). The only difference is a patch bump
  of `futures-channel/-core/-task/-util` from 0.3.32 to 0.3.34, which `gpui-pre`
  requires (`futures ^0.3.34`). No feature changed. No crate was added to the Windows
  graph, so the windows-msvc TUI build stays pure-Rust. `regex-automata` also moved
  from 0.4.14 to 0.4.18 in the lock (`globset`, pulled in by gpui-component's
  rust-embed features, needs it), but it is not in the root package's resolved tree.
- `desktop/` sets `[package.metadata.dist] dist = false`. Without it, cargo-dist
  (`members = ["cargo:."]`) would begin releasing the GUI.
- CI's `cargo fmt --all -- --check` now formats `desktop/` as well. It passes. Nothing
  in CI builds the GUI yet.

## Licences

The scan uses `cargo metadata` over the full resolve, **all target platforms**, and
normal + build edges walked from `flightdeck-desktop`. That covers 1003 packages,
including the TUI library's own graph. (`cargo tree -p flightdeck-desktop -e normal
--prefix none` lists 752 unique lines for the host and 1138 with `--target all`.)

**No crate is licensed GPL-only, AGPL, LGPL-only or SSPL.** None of Zed's GPL crates
(the editor, workspace, terminal and similar) are in the graph. Only `gpui-pre-*` and
its Apache-2.0 helper crates come from Zed.

Licence expressions found (count), in order of frequency:
MIT OR Apache-2.0 in its various spellings (437 + 97 + 49 + 11 + 4 + 1 = 599), MIT (233), Apache-2.0 (40),
Zlib OR Apache-2.0 OR MIT (21), Unicode-3.0 (18), BSD-3-Clause (10),
MIT OR Apache-2.0 OR Zlib (10), Unlicense OR MIT (8+2),
Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT (5), ISC (5),
Apache-2.0 OR ISC OR MIT (4), BSD-2-Clause (4), Zlib (4), CDLA-Permissive-2.0 (3),
CC0-1.0 (3), MPL-2.0 (3), BSD-3-Clause OR Apache-2.0 (2), 0BSD (2),
BSD-2-Clause OR Apache-2.0 OR MIT (2), MIT OR Apache-2.0 OR LGPL-2.1-or-later (2),
and one each of: (MIT OR Apache-2.0) AND NCSA, MIT AND BSD-3-Clause, MIT OR MPL-2.0,
the aws-lc-sys ISC/Apache/MIT/BSD-3 compound, BSD-2-Clause OR MIT OR Apache-2.0,
Apache-2.0 OR BSL-1.0, Apache-2.0 AND ISC, (MIT OR Apache-2.0) AND Unicode-DFS-2016,
MIT AND Unicode-DFS-2016, BSD-2-Clause OR Apache-2.0, CC0-1.0 OR Apache-2.0,
MIT OR BSD-3-Clause, bzip2-1.0.6, (MIT OR Apache-2.0) AND Unicode-3.0,
(Apache-2.0 OR MIT) AND BSD-3-Clause, 0BSD OR MIT OR Apache-2.0,
Apache-2.0 OR GPL-2.0-only, WTFPL, MIT OR Apache-2.0 OR CC0-1.0,
ISC AND (Apache-2.0 OR ISC), CC0-1.0 OR MIT-0 OR Apache-2.0.

Items to look at by hand:

| Crate | Licence | Via | Verdict |
| --- | --- | --- | --- |
| `self_cell` 1.3.0 | Apache-2.0 **OR** GPL-2.0-only | cosmic-text → gpui-pre-wgpu (Linux) | Fine. We take it under Apache-2.0. |
| `r-efi` 5.3.0 / 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1+ | getrandom (UEFI only). Already in the TUI graph | Fine. We take MIT. |
| `termina` 0.3.3 | MIT OR MPL-2.0 | Already in the TUI graph | Fine. We take MIT. |
| `cbindgen` 0.28.0 | MPL-2.0 | build-dependency of gpui-pre-apple (macOS) | A build tool. It is not linked into the binary. |
| `dwrote` 0.11.5 | MPL-2.0 | zed-font-kit (Windows DirectWrite) | Linked. MPL-2.0 is file-level copyleft: we can ship it unmodified in a closed or MIT binary as long as we point to its source. |
| `option-ext` 0.2.0 | MPL-2.0 | dirs-sys → dirs → zed-font-kit | Same as dwrote. |

The scan script is at `scratchpad/m0s1/licences.py` for this session (it was not
committed). Promoting it, or `cargo-deny` with a `[licenses]` allow-list, into CI is
a follow-up.

## Build prerequisites per OS

### macOS (verified)
- Xcode or the Command Line Tools: clang for `bindgen` and the macOS SDK for AppKit
  and Metal.
- **The Metal shader compiler is NOT required by default.** From Xcode 26 on it is a
  separate download (`xcodebuild -downloadComponent MetalToolchain`). Without it,
  gpui-pre-apple's build script fails with "missing Metal Toolchain". That is why the
  crate's default feature `runtime-shaders` makes GPUI compile its shaders at
  launch. **Release packaging** should build with `--no-default-features` on a
  machine that has the Metal Toolchain, so that precompiled shaders ship (not
  verified here: that machine did not have the toolchain).
- Clean build on an M2 Pro (10 cores): debug **89 s**, release **206 s**. Binary size:
  debug 79 MB, release 17 MB (no LTO or strip).

### Linux (NOT verified; no Linux machine was available)
From the graph (`xkbcommon`, `x11rb`, `wayland-*`, `yeslogic-fontconfig-sys`,
`freetype-sys`, `ash`/`wgpu`) and Zed's own Linux setup script. Debian/Ubuntu names:

```sh
sudo apt install build-essential pkg-config cmake clang \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libx11-xcb-dev libxcb1-dev libfontconfig-dev libfreetype-dev
# at run time: a Vulkan loader + driver (wgpu renders through Vulkan)
sudo apt install libvulkan1 mesa-vulkan-drivers
```

Fedora: `libxkbcommon-x11-devel wayland-devel libxcb-devel fontconfig-devel
freetype-devel vulkan-loader`. The TUI's own needs (a C compiler for bundled SQLite and
aws-lc) are unchanged. Unknown: whether a headless CI runner can *open* a window
(it would need Xvfb or a Wayland compositor, plus lavapipe for Vulkan).

### Windows (NOT verified; no Windows machine was available)
- The MSVC toolchain **with the Windows 10/11 SDK**. gpui-pre-windows' build script
  compiles its HLSL shaders with **`fxc.exe`** from the SDK and panics if it cannot find
  it (`GPUI_FXC_PATH` overrides the lookup). The `windows-manifest` feature embeds an
  application manifest through `embed-resource`, which uses `rc.exe` from the SDK.
- This means the GUI, **unlike the TUI**, is not a pure-Rust build on Windows and
  cannot use the self-contained windows-gnu setup. The TUI's Windows build is
  unaffected (see "Lock impact").
- Rendering is DirectX 11 with DirectWrite.

## What M0 does and does not do

- Opens one themed 1280×800 window (minimum 900×560). It has a 44px titlebar (on
  macOS: transparent, traffic lights inset, the app moves the window itself on drag,
  and a double-click follows the system setting; elsewhere: native decorations, with
  our bar as a toolbar underneath), a 272px sidebar with an "AGENTS" header and an
  empty state, a placeholder main well, and a 30px status bar. None of it shows real
  data.
- Every colour is a semantic token in `src/theme.rs`. Tests check that the four
  status colours differ in luminance (contrast ≥ 1.1 between every pair), that status
  glyphs reach ≥ 3:1 on the sidebar and on raised surfaces, and that body and muted
  ink reach ≥ 4.5:1 on every surface.
- The fonts are the system UI font and the default mono font. Geist and Geist Mono
  (OFL) are not bundled yet.
- Quit: ⌘Q on macOS, Ctrl+Q elsewhere. Closing the last window also quits.

## Not verified (said plainly)

- **Linux and Windows builds and runs were not attempted.** The prerequisites above
  come from reading the dependency graph and the build scripts, not from running them.
- **No screenshot.** `screencapture` failed ("could not create image from display")
  because this session has no Screen Recording permission. The window's existence
  was checked with `CGWindowListCopyWindowInfo` instead: a layer-0 window owned by
  `flightdeck-desktop`, 1280×800 at (260,162), on screen, in both debug and release
  builds. The process ran for 5 s with no stderr output and was then killed. **Nobody
  has looked at the pixels yet.** The traffic-light inset (`16,15`) and the 84px
  leading inset are estimates to check by eye.
- The precompiled-shader release path (`--no-default-features`).
- gpui-component widgets rendered with our palette. `theme::init` maps our tokens
  onto its theme, but no stock widget is drawn in M0.
