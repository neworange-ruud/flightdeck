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

## Keyboard parity (S3)

Written for beads issue `remote-control-bmej.1.3`. Code: `desktop/src/keys/`
(a library module, `flightdeck_desktop::keys`). Tests: `desktop/src/keys/tests.rs`.

### How it works

- **Bindings come from the table.** `keys::register(cx, keymap)` enumerates
  `Keymap::bindings_in(context)` and binds each trigger's exact chord. No chord is
  written by hand in the desktop crate. `chord_to_gpui` spells a crate `Chord` the
  way GPUI does (`ctrl-g`, `alt-up`, `shift-escape`, `f2`, `cmd-v`), and
  `chord_from_keystroke` goes the other way for typed keys.
- **One action type.** GPUI dispatches by Rust type and `actions!` needs a type for
  each name in source, which would be a second, hand-written list. Instead there is
  one `KeymapAction { id }` with a hand-written `Action` impl. Its `name()` is the
  entry's `gpui_action_name()` (`flightdeck::OpenPalette`) and `partial_eq`
  compares ids, so `keystroke_text_for` and `bindings_for_action` still tell
  entries apart. The view registers a single `on_action::<KeymapAction>` handler.
- **Key contexts** are the table's context names. `"Global"` goes on the window root
  (`app.rs` does this now), `"Terminal"` on the focused terminal element, and
  `"App"` on the element that has focus in app-command mode. Two rules come from
  how GPUI matches contexts:
  - Global bindings get a named context, not `None`. GPUI ranks a context-less
    binding *above* every context, so it would take Alt-Left from a text field
    inside an overlay.
  - `"App"` must never be an ancestor of `"Terminal"`. GPUI matches a context
    anywhere on the focus path, so bare Up and Ctrl-n would fire inside a terminal.
- **Leniency.** The TUI's `Trigger::tolerate` (Ctrl-Shift-g still opens the palette,
  Alt-Shift-Up still switches tabs) is not expanded into extra GPUI bindings. The
  key-down fallbacks (`terminal_key_down`, `app_key_down`) look the chord up in the
  table instead, so the results match the TUI.
- **Terminal mode, unbound keys.** `terminal_key_down` returns what to do with a key
  no binding claimed:
  - `Action`: a lenient table match.
  - `Pty(bytes)`: the key goes to the PTY, encoded with `encode_pty`, so arrows are
    always CSI.
  - `Text`: printable text. The handler does nothing and lets the platform input
    handler deliver the character.
  - `Ignore`: Cmd shortcuts, and keys like Insert that have no chord.
- **Text and IME.** Printable keys are not encoded on key-down. They go through
  GPUI's `EntityInputHandler`, so CJK composition works. `ImeState` holds the
  composition (marked text) as a preview and sends nothing. Only committed text
  (`replace_text_in_range`) reaches the PTY, as UTF-8. The doc comment on
  `ImeState` has the method-by-method delegation the terminal element should copy.
  An abandoned composition (`unmark_text`) is dropped rather than typed, which
  matches what Zed's terminal does.
- **Paste.** `paste_bytes(text, bracketed)` is the TUI's encoder. It moved from a
  private function in `src/lib.rs` to `flightdeck::app::keymap::encode_paste`, so the
  TUI, the phone relay (`remote::commands::encode_reply`) and the GUI all share one
  implementation. It turns newlines into CR, and adds `ESC[200~` … `ESC[201~` when
  the app has turned DECSET 2004 on.
- **Cmd on macOS** is never used for a FlightDeck chord, except the table's Cmd-V
  paste, which is bound exactly. A Cmd keystroke that is not bound returns
  `Ignore`/`None` and propagates to the platform (Cmd-Q, Cmd-comma, Cmd-C). This
  means the TUI's leniency does not apply to Cmd chords: Cmd-Shift-V is not a
  paste.
- **F2.** It follows `KeymapOptions::use_f2_to_leave_focus`. When F2 is on, it
  matches with any modifier held, as in the TUI, and Alt/Shift-Esc goes to the PTY.
  The GUI does not read that config setting yet (it uses the default table;
  `remote-control-9diy`).

### The macOS Option policy

GPUI reports a keystroke as the key on the keycap (`key`), the modifiers held, and
the character the press would type (`key_char`). Option+1 is `key "1"`, `alt`,
`key_char "¡"`. Bindings match on `key`, so **Option always works as Alt for bound
chords**: Alt-1..9, Alt-o, Alt-h, Alt-Esc and Alt-arrows fire whatever glyph the
layout composes. The TUI can't do this, which is why it asks users to turn on "Use
Option as Meta" in their terminal.

For an **unbound** Option+key in Terminal mode, `OptionKey` decides:

- `Compose`, the macOS default, types the composed character (`∫`, a dead-key
  accent, `@` on a German layout). When the platform reports no composed character,
  it falls back to Meta.
- `Meta` sends `ESC` + key. That is byte-for-byte what the TUI sends when the host
  terminal has Option as Meta turned on, and it keeps readline's Meta-b/f/d.

**Why Compose by default:** Non-US Mac layouts put characters a shell needs behind
Option: `@ [ ] { } | \ ~` on German, French and Nordic layouts. Under Meta those
users could not type them into an agent. The reason the TUI needed Meta was to reach
FlightDeck's own Alt chords, and that reason no longer applies because those chords
are bindings. Compose is also the default in macOS Terminal.app and in Zed's
terminal. The cost is readline Meta-word motions for US-layout users, which is why a
setting for it is filed (`remote-control-9diy`). On Linux and Windows the policy
makes no difference. Alt does not compose there (Windows AltGr arrives with
`prefer_character_input`, which is honoured first), so both policies send `ESC` +
key.

### Chord matrix

The "macOS" column is the result on this machine (Darwin 27, Apple M2 Pro).
`cargo test -p flightdeck-desktop` drives each keystroke through GPUI's real keymap,
key-context matching, action dispatch, key-down listeners and input handler, using
GPUI's headless test platform (`gpui/test-support`, `VisualTestContext::
simulate_keystrokes`). The test window has the app's shape: a `"Global"` root with
sibling `"App"` and `"Terminal"` panes. The platform layer (AppKit's event
translation and `NSTextInputContext`) is the one piece not exercised. See "Not
verified" below.

**Linux and Windows: not run.** No Linux or Windows machine was available. The
tests are OS-independent (every table variant is built on every OS, and Cmd is
spelled per-OS), but they have never been compiled or executed there. That is
`remote-control-hu6n`.

"passthrough" means the chord is not bound in that mode, and the Terminal pane wrote
exactly `encode_pty(chord)`, the TUI's bytes (shown in brackets). "nothing" means
App mode ignores the key.

| Chord | Entry | Context | GPUI binding | App mode | Terminal mode | macOS | Linux | Windows |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Ctrl-g | OpenPalette | Global | `ctrl-g` | action | action | pass | not run | not run |
| Ctrl-q | Quit | Global | `ctrl-q` | action | action | pass | not run | not run |
| F1 | OpenHelp | Global | `f1` | action | action | pass | not run | not run |
| Alt-h | OpenHelp | Global | `alt-h` | action | action (also as `alt-h->˙`) | pass | not run | not run |
| Shift-Left / Shift-Right | SwitchProjectPrev / Next | Global | `shift-left` / `shift-right` | action | action | pass | not run | not run |
| Alt-Up / Alt-Down | AgentTabPrev / Next | Global | `alt-up` / `alt-down` | action | action | pass | not run | not run |
| Alt-Left / Alt-Right | TerminalTabPrev / Next | Global | `alt-left` / `alt-right` | action | action | pass | not run | not run |
| Alt-1 .. Alt-9 | JumpToAgentTab1..9 | Global | `alt-1` .. `alt-9` | action | action (also as `alt-1->¡`, `alt-9->ª`) | pass | not run | not run |
| Alt-o | OpenWorktreeInFileManager | Global | `alt-o` | action | action (also as `alt-o->ø`) | pass | not run | not run |
| Up / Down | AgentTabPrev / Next | App | `up` / `down` | action | passthrough (`ESC[A` / `ESC[B`) | pass | not run | not run |
| Left / Right | TerminalTabPrev / Next | App | `left` / `right` | action | passthrough (`ESC[D` / `ESC[C`) | pass | not run | not run |
| Ctrl-n | NewAgentTab | App | `ctrl-n` | action | passthrough (`0x0e`) | pass | not run | not run |
| Ctrl-p | PushBranch | App | `ctrl-p` | action | passthrough (`0x10`) | pass | not run | not run |
| Ctrl-u | PullBase | App | `ctrl-u` | action | passthrough (`0x15`) | pass | not run | not run |
| Ctrl-f | FinishLocalMerge | App | `ctrl-f` | action | passthrough (`0x06`) | pass | not run | not run |
| Ctrl-k | CloseAgentTab | App | `ctrl-k` | action | passthrough (`0x0b`) | pass | not run | not run |
| Ctrl-t | NewChildTerminal | App | `ctrl-t` | action | passthrough (`0x14`) | pass | not run | not run |
| Ctrl-w | CloseChildTerminal | App | `ctrl-w` | action | passthrough (`0x17`) | pass | not run | not run |
| Ctrl-b | ToggleSplitView | App | `ctrl-b` | action | passthrough (`0x02`) | pass | not run | not run |
| Enter | FocusTerminal | App | `enter` | action | passthrough (`\r`) | pass | not run | not run |
| Ctrl-s | SetManualStatus | App | `ctrl-s` | action | passthrough (`0x13`) | pass | not run | not run |
| Ctrl-r | RestartAgent | App | `ctrl-r` | action | passthrough (`0x12`) | pass | not run | not run |
| Ctrl-v | Paste | Terminal | `ctrl-v` | nothing | action | pass | not run | not run |
| Cmd-V (macOS table) | Paste | Terminal | `cmd-v` | nothing | action | pass | not run | not run |
| Alt-Esc (macOS table) | FocusApp | Terminal | `alt-escape` | nothing | action | pass | not run | not run |
| Shift-Esc (Linux/Windows table) | FocusApp | Terminal | `shift-escape` | nothing | action | pass (table built here) | not run | not run |
| F2 (`use_f2_to_leave_focus`) | FocusApp | Terminal | `f2` | nothing | action, with any modifier; then Alt-Esc is passthrough (`ESC`) | pass | not run | not run |

Every row is checked for all 8 combinations of the table's options (F2 on/off ×
Shift-Esc/Alt-Esc × Cmd-V on/off). Rows that depend on the OS are marked. The test
fails when the table gains, loses or re-spells a chord, until the expected matrix in
the test (and this table) is updated.

Also covered:

- **Every Ctrl-letter in App mode** (a–z) resolves to exactly the table's App-mode
  entry or to nothing, and never types.
- **Leniency matches the TUI's `map_key`:** Ctrl-Shift-g (both modes), Alt-Shift-Up,
  Ctrl-Alt-1, Ctrl-Alt-n (App) and Ctrl-Shift-v are all compared against the TUI's
  own result for the same crossterm event.
- **Cmd is left to the platform:** Cmd-Q, Cmd-comma, Cmd-C, Cmd-A, Cmd-Shift-G and
  Cmd-Up do nothing in either mode.
- **Unbound keys in Terminal mode, byte for byte with the TUI.** 40 representative
  keys give the same bytes as both `encode_pty` and the TUI's own
  `tui::input::encode_key` on the crossterm event. The keys: text (`a`, `A`, space,
  digits), Enter, Esc, Tab, Shift-Tab (`ESC[Z`), Backspace, Delete, the arrows
  (always CSI, including Shift- and Ctrl-arrows), Home, End, PgUp, PgDn, F2–F5, F12,
  F13 (sends nothing), Ctrl-a/c/d/n/r/z, Ctrl-Alt-a, and Alt-b/f/Shift-a/Backspace/PgUp
  under Meta. A sweep then presses **every** key (letters in both cases, digits,
  punctuation, named keys, F1–F24) with **every** Shift/Ctrl/Alt combination that is
  unbound in Terminal mode, which is about 800 presses, and checks each against both
  encoders.
- **Option policy:** Compose types `∫`, `@` and a dead-key `´` as UTF-8. Meta sends
  `ESC b` / `ESC l` for the same keystrokes. Compose with no composed character sends
  `ESC b`. Ctrl-Option is never treated as text.
- **IME:** a Japanese composition (`n` → `に` → … → `にほん`, then commit `日本`)
  goes through `ElementInputHandler`, the adapter GPUI gives the platform. Nothing
  reaches the PTY until the commit, and then the bytes are the UTF-8 of `日本`. An
  abandoned composition types nothing, and neither does an emptied one.
  `text_for_range` returns the requested slice in UTF-16 units.
- **Typing and paste:** `simulate_input("git status -sb")` arrives through the input
  handler byte for byte. A two-line paste gives `ESC[200~echo 1\recho 2ESC[201~` with
  bracketed paste on, and raw text with CR line breaks when it is off.

Test counts: 21 desktop library tests (`keys::tests`) plus the existing 4 theme
tests. Three `encode_paste` tests moved with the function into
`src/app/keymap/tests.rs`, so the root library count is unchanged at 1586.

`test-support` pulls `proptest` and a handful of small crates into `Cargo.lock`
(`bit-set`, `bit-vec`, `convert_case`, `proptest-macro`, `quick-error`,
`rand_xorshift`, `rusty-fork`, `unarray`, `wait-timeout`). They are dev-dependencies
of the desktop crate only. The root package and the shipped GUI binary do not get
them.

### Not verified (said plainly)

- **Linux and Windows: nothing was run** (`remote-control-hu6n`). This includes
  Windows AltGr through `prefer_character_input`, how Linux reports `key_char` for
  Alt+letter, and ibus/fcitx commits.
- **No physical keyboard on macOS** (`remote-control-6xdd`). The headless platform
  replays GPUI's dispatch order but not AppKit's. It does not cover: which events
  AppKit sends to `NSTextInputContext` first (an active CJK input source takes
  printable keys before the keymap does), real Option composition and dead keys
  under a German layout, the Cmd-Q/Cmd-comma path through a real app menu, press-and-hold,
  or a real IME's candidate window (`bounds_for_range` belongs to the terminal
  element, which does not exist yet).
- **Nothing is wired end-to-end.** No terminal element or PTY exists in the GUI yet.
  The harness in `tests.rs` shows the wiring the terminal element should copy. In the
  app, only Quit does anything. Every other entry is claimed by the root's action
  handler, so a FlightDeck chord never reaches a PTY, but it does nothing yet. The
  root has no focus target in M0, so the Global context is not on the focus path
  until a focused view exists.

## Terminal emulator (spike S2, `remote-control-bmej.1.2` / `.2.3`)

Verified 2026-09-29 on the same machine as above. This section compares the two
candidate emulators behind the new grid seam, records the recommendation, and
says how the spike terminal was checked.

### The seam

`src/terminal/grid/` in the core defines two traits. `GridView` is read-only: size,
`visit_row` (per cell: grapheme, fg/bg, bold/dim/italic/underline/inverse/
strikethrough, wide or continuation), cursor (position, shape, blink, visible),
modes (alt screen, mouse mode + encoding, bracketed paste, app cursor/keypad) and
the scrollback offset. `TerminalGrid: GridView` adds feed, `tick`, query replies,
resize, scrollback length/offset and the selection.

The emulator is picked in one place, `Emulator` plus `TUI_EMULATOR`. `vt100_grid.rs`
and `alacritty_grid.rs` are the only files that name an emulator crate.
`testing::FakeGrid` covers tests that need no parser.

The TUI renderer, `Terminal`, the mouse encoders and the desktop element all go
through the traits. Web replay still streams raw PTY bytes and touches none of this.

### Comparison

Every row marked "fixture" is asserted in `src/terminal/grid/fixtures.rs`. The
shared contract runs against both emulators, and a second set of tests pins down
where they differ.

| | vt100 0.16.2 (TUI today) | alacritty_terminal 0.26.0 |
| --- | --- | --- |
| Licence | MIT | Apache-2.0. We use only the emulation core (`Term` + vte). Its `tty`/`event_loop` stay unused, because our PTY stays behind `PtySession`. |
| Graph added to the root | none (already there) | 13 crates on windows-msvc (aho-corasick, regex-automata/-syntax, polling, piper, miow, concurrent-queue, crossbeam-utils, fastrand, futures-io, cursor-icon, home, itself). darwin and linux each add 9: the same set minus the six that are Windows-only
or already present, plus rustix-openpty and signal-hook. **No `-sys` crate and no `cc` on any target.** Checked with `cargo tree -p flightdeck -e normal,build --target …` before and after, so the Windows TUI build stays pure Rust. |
| Scrollback | yes, primary screen only. The offset API is public but the filled length is private (`Vt100Grid` measures it by clamping). | yes, primary only. `display_offset` / `history_size`. |
| Selection | none. FlightDeck's own scroll-stable `Selection`. | Native selection (simple, semantic, line, block; rotates with scroll) is available but **not used yet**. Both backends share FlightDeck's `Selection` through the trait. |
| Reflow on resize | **no.** Rows are truncated and the cut text is lost (fixture). Shrinking through a wide character can also panic, which `Vt100Grid` contains by rebuilding the parser and losing the screen. | **yes.** Soft-wrapped lines rewrap both ways and the cursor line stays put (fixture). |
| Wide / emoji / combining | CJK and emoji Wide, combining marks join the cell (fixture) | same (fixture) |
| Emoji + VS16 (`❤️`) | 1 column (fixture) | 1 column (fixture). Fonts draw it 2 wide, which is the gap the `*-alacritty-terminal` forks on crates.io patch. |
| Alternate screen | `?1049`, `?47` | `?1049` only. `?47` is ignored (fixture). |
| Mouse modes | `?9` X10, `?1000`, `?1002`, `?1003`; encodings `?1005`, `?1006` | `?1000`, `?1002`, `?1003`, `?1005`, `?1006`, also focus `?1004` and alternate scroll `?1007`. **No X10 `?9`** (fixture). |
| Bracketed paste, DECCKM, DECKPAM, DECTCEM | yes (fixture) | yes (fixture) |
| Cursor shape (DECSCUSR) | not modelled. Recovered through vt100's `unhandled_csi` callback (fixture). | native, with blink (fixture) |
| Synchronized output `?2026` | ignored, so half-drawn frames show | buffers until ESU or a 150 ms timeout (`tick` flushes it) (fixture) |
| Query replies | none of its own. FlightDeck answers DSR 6 by hand. | DSR 5/6, DA1/DA2, OSC 4/10/11/12 colour queries (fixture: OSC 11 answers with the theme background). The pixel-size query (`CSI 14 t`) is not answered because no pixel size is wired. |
| OSC 4/10/11 palette sets | ignored (fixture) | applied to cells (fixture) |
| SGR extras | no strikethrough, conceal or underline variants | strikethrough (fixture), conceal, double/curly/dotted/dashed underline, underline colour, OSC 8 hyperlinks |
| Kitty keyboard protocol | no | optional (off; config) |
| Robustness | panics in 0.16.2 (see `Vt100Grid::process`), contained by `catch_unwind` + rebuild. 0.16.2 is the newest release. | a 4 KiB noise feed plus wide characters at tiny sizes does not panic (fixture) |
| Upstream | occasional releases | Alacritty and Zed both depend on it |

Real agents through the same pipeline (`--dump-grid`, both emulators, 30x100, no
prompt sent):

- `claude` 2.1.284: the trust dialog, then the main screen (logo quadrants, rules,
  prompt, footer).
- `codex` 0.151: the update prompt.
- `opencode` 1.18.33: alt screen, `?1003` + SGR mouse, bracketed paste, blinking
  cursor.

Every screen parsed cleanly. For each agent, the two emulators' dumps differ only
in what the agent printed on that particular launch (opencode's random tip and
placeholder, a claude announcement banner). The layout, modes and cursor match.
The first codex run showed the cursor blinking on vt100 and steady on alacritty,
from DECSCUSR 0. `Vt100Grid` now treats 0 as the steady default, as alacritty
does (fixture).

### Recommendation

**alacritty_terminal for the desktop app, which is what the GPUI element uses
now.** It reflows on resize, which a resizable GUI window hits constantly while
vt100 loses text there and can panic. It honours synchronized output, which
removes the redraw flicker from Claude Code and opencode frames. It answers the
terminal queries agents use to detect capabilities and choose a light or dark
theme. It tracks the SGR and OSC features that agents emit. And it is maintained
by two large terminal projects. Its gaps (`?47`, X10 mouse) are legacy modes that
none of the agent CLIs above use.

**The TUI stays on vt100 for now** (`TUI_EMULATOR = Emulator::Vt100`). Switching
it is the one line in `grid/mod.rs`, but it is not a pure swap:

1. In the TUI a real host terminal sits outside FlightDeck. Answering OSC 11/DA
   with FlightDeck's own values would misreport that terminal. The replies would
   have to be proxied or suppressed per query.
2. Reflow and sync buffering change what users see during resizes and repaints.
3. The TUI's render tests pin vt100 behaviour.

That is a follow-up issue, not part of this spike.

**Why the alacritty impl lives in the core, not in `desktop/`:** the conformance
fixtures run both backends in the root gate (`cargo test -p flightdeck --lib`),
swapping the TUI stays a one-module change, and the root stays pure Rust (verified
above). The cost is compile time for the TUI's library plus alacritty's
`tty`/`event_loop` modules, which are compiled but never linked in.

Zed's `terminal_view` (GPL) was not read or copied. The element uses only GPUI's
public API. For example, the per-glyph advance in `shape_line`'s `force_width`
is a GPUI parameter.

### The spike terminal

- `cargo run -p flightdeck-desktop -- --spike-terminal [cmd args…]`: one
  900x600 window with one terminal running `cmd` (default `$SHELL`, PowerShell on
  Windows), built on `Terminal::spawn(PortablePtyBackend, Emulator::Alacritty, …)`.
  The process gets `TERM=xterm-256color` and `COLORTERM=truecolor`, because a GUI
  launched from the Finder has no `TERM` to pass down.
- `--dump-grid [--emulator vt100|alacritty] [--wait-ms N] [--size RxC] [cmd…]`:
  the same pump and layout with no window. It prints the grid, modes, cursor and
  the frame layout.
- With `--features spike-snapshot` (off by default: it turns on GPUI's
  test-support, which pulls proptest and other extra crates into `Cargo.lock` but
  not into the default build), three more flags are accepted:
  `--spike-snapshot PATH [--spike-keys "a b enter ctrl-c"] [--spike-wait-ms N]`.
  They type through the real key handler and then write the window's own rendered
  frame (`Window::render_to_image`) as a PPM file. This needs no Screen Recording
  permission.

How the element draws:

- Menlo 13px on macOS (Consolas on Windows, DejaVu Sans Mono elsewhere). The cell
  is the advance of `m` wide and `max(ascent+descent, 1.25em)` tall.
- ASCII spans are shaped with a forced cell advance. Other spans are shaped alone
  and placed at their column.
- Box drawing (light/heavy lines, rounded corners), block elements, quadrants and
  shades are drawn as geometry. Doubles and dashes fall back to the font.
- Colours come from new `theme.rs` tokens: `terminal_ansi[16]`, cursor and
  selection. The 256-colour cube and truecolour are passed through.
- The cursor is a filled block when focused and an outline otherwise, or a bar or
  underline per DECSCUSR.
- Resizing the element resizes the grid and the PTY.
- Keys are lifted into `Chord` and encoded by `keymap::encode_pty`, so arrows are
  always CSI. Paste is Cmd+V on macOS and Ctrl+Shift+V elsewhere, honouring
  bracketed paste.
- Mouse events are forwarded when the program asked for them (Shift overrides).
  Otherwise the mouse drives local selection (copied on release) and scrollback.
  On the alt screen without mouse reporting, the wheel sends arrow keys.

### Verified, and how

- **Pixels, live, via `--spike-snapshot`** (macOS, debug build). I looked at each of
  these frames:
  - the fixture line (`\e[31mred` … truecolour, `┌─┬─┐` boxes, rounded box, heavy
    lines, the `▐▛██▜▌` quadrants, shades, `😀 宽字 é`): every glyph sits on its
    column, box joints meet, the wide glyphs span two cells;
  - interactive bash with typed input (`echo hello world`, Enter, Ctrl-A, seven
    Right arrows, insert, Ctrl-E, Backspace), which produced the expected command
    line and output;
  - vim on the alternate screen after `j j j V j j`: syntax colours, line numbers,
    visual-line highlight, and a window-sized grid, which confirms the 24x80 → window
    resize reached the PTY;
  - the first screens of opencode (its truecolour background panels and block logo),
    claude (the quadrant logo drawn as solid geometry) and codex.
- **Window existence without the feature:** `--spike-terminal bash -lc 'printf …;
  sleep 8'` opened a 900x600 layer-0 window (`CGWindowListCopyWindowInfo`) with no
  stderr output.
- **Headless:** `--dump-grid` on the fixture, vim, less and the three agents with
  both emulators, and `a_real_pty_reaches_the_frame_layout` (a real `/bin/sh` PTY →
  grid → layout, in `cargo test`).
- **Tests only (not exercised live):** selection highlight and copy (layout test
  `selection_becomes_one_span_per_row`, plus the grid fixtures), mouse-report
  bytes, paste bracketing, the unfocused and bar/underline cursors, and scrollback
  scrolling in the window.
- **Not verified:** Linux and Windows builds or runs of the desktop element;
  IME / dead keys (the spike reads `key_char` only). Performance on a large,
  busy grid is measured in "Performance (S4)" below.

## Performance (S4, `remote-control-bmej.1.4`; damage tracking `remote-control-bmej.3.9`)

Measured 2026-09-29 on the M2 Pro (10 cores, macOS 27.0, 120 Hz display), release
builds. **Linux and Windows were not measured**: no machine was available. The
procedure below should run unchanged on Linux. On Windows `perf.py` does not
run (it drives PTYs with Python's Unix-only `os.openpty`), but `flightdeck-desktop
--bench …` does, and the TUI side has to be timed by hand.

Other agents' builds were running on the machine at the same time (load average
between 5 and 135 on 10 cores). Throughput swings with that load by up to 3x
between runs, and it swings for both front-ends and for the raw PTY alike. So
every throughput number below sits next to a "PTY ceiling" taken right before
it, and the before and after GUI builds were run interleaved. Latency and idle
numbers stayed within about 10 % across runs.

### How to re-run

```sh
cargo build --release -p flightdeck --bin flightdeck
cargo build --release -p flightdeck-desktop
python3 desktop/benches/perf.py all               # ~12 minutes
python3 desktop/benches/perf.py gui-latency tui-latency --keys 300
python3 desktop/benches/perf.py gui-throughput --rounds 3 --gui path/to/other/flightdeck-desktop
target/release/flightdeck-desktop --bench idle --ticker --fill --terminals 4 --seconds 15
```

`flightdeck-desktop --bench SCENARIO` (desktop/src/terminal/bench.rs) opens a
real 1040x700 window, runs `/bin/sh` on real PTYs, drives the scenario and
prints its numbers. The window holds a 40x130 grid of Menlo 13, close to the
42x130 terminal pane the TUI gets in a 160x50 host. A `Probe` on the view and
the element records the timestamps. With no probe attached (every normal run)
each hook is one `None` check. `perf.py` runs the same workloads against the
real TUI binary: `flightdeck -I` in a 160x50 PTY, with a throwaway HOME and git
repo and an agent whose command is `/bin/sh`. It samples CPU and RSS of both
front-ends with `ps`.

### What each number is

- **Keystroke → glyph, GUI:** from GPUI's key-down dispatch reaching the view's
  handler, to `write_input` returning, to the first PTY poll that parses the
  echo, to the end of `paint` in the first frame whose prepaint began after
  that parse. GPUI hands the scene to Metal in the same call, so this is "frame
  ready to present". The display's scan-out (up to 8.3 ms at 120 Hz) is not
  included. The key goes into `cat > /dev/null`, so the tty echoes it, and keys
  are 30–70 ms apart at random so nothing phase-locks with the poll or the
  display.
- **Keystroke → glyph, TUI:** `perf.py` writes `ж` to the TUI's PTY and times how
  long until TUI output containing it arrives. That covers key read → PTY
  write → echo parsed → frame flushed, plus two PTY hops (tens of µs). The host
  terminal's own drawing is not included, just as scan-out is not included for
  the GUI.
- **Throughput:** from pressing Enter on `CMD; printf '%s-%s\n' FD DONE` in an
  idle shell to the painted (GUI) or flushed (TUI) frame that shows `FD-DONE`.
  `CMD` is `cat` of a 50 MiB log-like file (100-column lines, every fifth with
  an SGR colour), or `seq 1 2000000` (20 MB, one `write` per line because stdout
  is a tty). "PTY ceiling" is a tight Python `os.read` loop on the same command,
  with no parsing and no drawing.
- **Idle CPU:** cumulative CPU time of the app process (`ps -o time`) over 30 s,
  after 5 s of warm-up, as a % of one core. Child shells are not counted. RSS
  is the mean of 1 s samples. "Ticker" means `while true; do date; sleep 1; done`
  in every terminal. With 4 terminals the GUI shows all four (2x2). The TUI's
  four are the agent plus three child shells (Ctrl-T), and only one is visible.
- **Scroll:** with 3000 coloured lines in the scrollback, scroll three lines per
  frame (driven from `on_next_frame`) for 10 s. Element CPU is prepaint start
  → paint end. The interval is from one paint end to the next.

### Results

Each cell gives the runs taken, separated by `·`.

| | GUI before | GUI after | TUI |
| --- | --- | --- | --- |
| Key → glyph p50 / p95 (4 runs) | 6.8–8.5 / 13.7–15.5 ms | **6.0–6.6 / 9.5–9.9 ms** | 54.7–58.6 / 58.4–65.8 ms (3 runs) |
| … write → echo parsed, p50 / p95 | 4.2–4.4 / 8.5–8.9 ms | 1.8–2.0 / 2.1–2.6 ms | n/a |
| … parsed → painted, p50 / p95 | 1.2–3.0 / 7.9–9.1 ms | 4.3–4.6 / 8.0–8.3 ms | n/a |
| `cat` 50 MiB, total (PTY ceiling) | 0.91 s (0.87) · 1.21 s (0.97) | 0.68 s (0.53) · 0.59 s (0.67) | 1.14 s (0.53) · 0.89 s (0.67) |
| `seq 1 2000000`, total (PTY ceiling) | 3.5 s (3.9) · 3.2 s (3.6) | 3.2 s (3.9) · 3.5 s (4.1) | 3.5 s (3.9) · 3.3 s (earlier run, no ceiling taken) |
| Idle CPU, 1 terminal | 1.55 · 1.78 % | **0.66 · 0.66 %** | 1.25 · 0.89 % |
| Idle CPU, 1 terminal + ticker | 1.29 · 1.49 % | **0.89 · 0.79 %** | 1.02 · 0.96 % |
| Idle CPU, 4 terminals | 2.28 · 3.10 % | **0.83 · 0.83 %** | 0.96 · 0.96 % |
| Idle CPU, 4 terminals + tickers | 2.32 · 3.01 % | **1.06 · 1.16 %** | 0.93 · 0.93 % |
| RSS, 1 terminal / 4 terminals with tickers | 89–102 / 106–107 MB | 100–103 / 106 MB | 9–10 / 11 MB |
| Frames/s while idle, 1 / 4 terminals | 5–6.5 / 18.5–19.2 | **0 / 0** | the TUI redraws every 50 ms |
| PTY polls/s per idle terminal | 117 | 38 (57 with a ticker) | 20 (one loop for all) |
| Scroll: element CPU p50 / p95 per frame | 0.60–0.61 / 0.64–0.66 ms | 0.66 / 0.71–0.73 ms | n/a |
| Scroll: frame interval p50 / p95 / max | 8.3 / 10.1–13.2 / 25–29 ms | 8.3 / 9.3 / 25 ms | n/a |
| One row changing on a full screen (`--fill`): element CPU p50 | 0.60 ms (re-lays all 40 rows; the scroll figure) | 0.45 ms (re-lays 1 row) | n/a |

Left out as outliers: one before-run of `cat` whose typed command was lost
(5.3 s, 429 bytes), and one TUI `seq` run caught by a load spike (13.1 s).

What the numbers say:

1. **Throughput is bounded by the PTY, not by either front-end.** Both finish
   within noise of the raw read loop. The GUI's parse (alacritty, about 150 MB/s:
   350 ms of UI-thread time for the 50 MiB) and its element (70–140 ms over
   60–120 frames) leave the UI thread idle most of the time. `seq` is bounded
   by `seq` itself (2 M tiny writes). Nothing needed changing here except
   bounding the worst case (see below).
2. **Latency was the poll interval.** Before, the echo waited half of the fixed
   8 ms poll on average, and a whole one at p95. Now the view polls every
   millisecond for 40 ms after input and reads the echo within about 2 ms
   (GPUI's timer granularity). The rest is waiting for the next display
   refresh, which no element can avoid. The TUI's 55 ms comes from its main
   loop: it reads the PTY, draws, then waits up to 50 ms for input
   (`POLL_TIMEOUT`), and the echo nearly always lands just after the read that
   follows the key.
3. **Idle cost was the repaint timer and the fixed poll rate.** The spike
   repainted every 150 ms in case a synchronized update had timed out, and each
   repaint redrew the whole window, so 4 idle terminals drew 18.5 frames/s. The
   cursor does not blink, so it was never the cause. Now an idle terminal draws
   no frames at all and polls 38 times a second. 4 terminals with tickers use
   1.2 % of one core, under the 2 % target and level with the TUI.
4. **The row cache helps less than dropping the idle frames.** GPUI already
   keeps last frame's shaped lines, so re-shaping an unchanged row was mostly a
   hash lookup. What remains per frame is submitting every glyph. On a full
   screen where one row changes, a frame costs 0.45 ms instead of 0.60 ms. When
   everything changes (scrolling, a flood), the cache's bookkeeping adds about
   0.05 ms per frame. Both are far inside an 8.3 ms frame.
5. **Memory:** the GUI's ~100 MB RSS is GPUI and Metal. It is about the same
   with one terminal or four, so the grids are a small part of it. The TUI's
   10 MB is low because the host terminal does the drawing for it.

### What changed in the element (`remote-control-bmej.3.9`)

- **Damage tracking.** `TerminalGrid::take_damage` (new, in the core seam)
  returns the rows that changed since the last call. The alacritty backend
  forwards `Term::damage`, which also covers the old and new cursor rows and
  reports scrolls, resizes and full-screen changes as `Full`. vt100 always says
  `Full`. `desktop/src/terminal/rowcache.rs` keeps each row's layout and shaped
  lines, and redoes a row only when:
  - it is damaged,
  - the cursor entered it, left it or changed shape on it (checked there as
    well, so correctness does not rest on the emulator's cursor bookkeeping),
  - or the size or palette changed (then every row).

  The selection is an overlay rebuilt each frame. `layout::layout_row` is the
  per-row half of the old whole-frame `layout`, which `--dump-grid` and the
  tests still use. The cache tests assert that the cached rows always equal a
  fresh whole-frame layout.
- **Repaint only on change.** The view notifies GPUI when:
  - a poll parsed bytes,
  - the emulator released a held synchronized update
    (`TerminalGrid::holds_output`, new),
  - or the process ended.

  The 150 ms repaint timer is gone. A key no longer repaints by itself unless
  it cleared a selection or jumped back from scrollback; its echo does the
  repainting.
- **Adaptive polling** (`desktop/src/terminal/cadence.rs`): every 1 ms for 40 ms
  after input, every 8 ms while output flows or a synchronized update is held,
  and every 25 ms when idle. So output a program starts on its own waits at most
  25 ms, half the TUI's loop. Input restarts the poll task, so fast polling
  starts at the keystroke rather than after the rest of an idle sleep.
- **At most one frame per refresh, and bounded work per poll.** GPUI already
  coalesces any number of notifies into one frame per display refresh, so a
  flood is laid out once per frame, not once per read. The parse was what had
  no bound. A poll now parses at most 1 MiB (`PARSE_BUDGET`, a few ms) and keeps
  the rest in a backlog, which is polled again after 1 ms. Output that piled up
  while the UI thread was busy is worked off with frames in between instead of
  in one long stall.

Not done, because the numbers do not call for it:

- skipping frames during a flood (the element takes about 1 ms of an 8.3 ms
  frame),
- reusing rows across a scroll (0.66 ms a frame),
- an event-driven PTY wakeup instead of polling. It would need a change to
  `PtySession` in the core, and at 25 ms the idle poll costs less than the
  TUI's own loop.

With several terminals in one window, GPUI redraws every element when any one
of them notifies. The others then only repaint cached rows. If that ever shows
up in a profile, the app shell can wrap each terminal in a cached view.
