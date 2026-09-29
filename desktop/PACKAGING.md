# flightdeck-desktop: CI and packaging

Covers beads `remote-control-bmej.6.1` (CI), `.6.2` (macOS), `.6.3` (Windows) and `.6.4` (Linux).

**Assumption (bmej.1.7 is not final):** the desktop app ships as a separate
`flightdeck-desktop` binary next to the `flightdeck` CLI, as it is built today. If the
launch layout changes (one binary, or the CLI launching the GUI), the bundle
executable names, the `.desktop` `Exec=`, the WiX component and the cask `app` stanza
change with it; nothing here depends on the CLI being installed.

## Verification status

| Piece | Status |
| --- | --- |
| `.github/workflows/desktop.yml` | Passes `actionlint`. **Never run**: CI cannot be run from this environment. |
| macOS `.app` (`scripts/desktop/macos-bundle.sh`) | **Built and launched locally, unsigned** (see below). |
| Codesign / notarize steps | Written, **not run** (no Developer ID identity here). |
| Homebrew cask template | Not audited or published. |
| Linux `.deb` / `.rpm` / AppImage | **Not run** (no Linux machine). Config only. |
| Windows MSI | **Not run** (no Windows machine). Config only; XML is well-formed. |

## CI (`.github/workflows/desktop.yml`)

Triggers: pull requests that touch `desktop/**`, `src/**`, `Cargo.toml`, `Cargo.lock`,
`scripts/desktop/**` or the workflow itself; pushes to `main` touching the same code
paths; and manual `workflow_dispatch`.

`check` job, on macOS, Ubuntu and Windows (`fail-fast: false`):

- `cargo clippy -p flightdeck-desktop --all-targets --locked -- -D warnings`
- `cargo test -p flightdeck-desktop --locked` (Linux under `xvfb-run -a`)
- macOS only: `cargo build -p flightdeck-desktop --features spike-snapshot --locked`
  and a `--release` build.
- Cargo is cached with `Swatinem/rust-cache` (own `prefix-key`, separate from `ci.yml`).

Per-OS prerequisites: Ubuntu installs the apt list from `NOTES-M0.md` (plus
`xvfb`, `libvulkan1`, `mesa-vulkan-drivers`); macOS relies on the default
`runtime-shaders` feature, so the missing Metal Toolchain does not matter; the
Windows runner image already has MSVC and the Windows SDK (`fxc.exe`, `rc.exe`).

Notes:

- **Display on Linux.** GPUI tests use `TestAppContext` on the headless test platform
  and should not open a window. `xvfb-run` is wrapped around the Linux test step as
  cheap insurance. If the first green run shows it is unnecessary, remove it.
- **Root CI is unchanged.** `ci.yml` keeps covering the TUI only (`default-members = ["."]`).
  Its `cargo fmt --all -- --check` job already formats `desktop/` as well (`--all`
  walks every workspace member), so no separate fmt job was added.
- **Path filters and required checks.** If `check (...)` is made a required status
  check in branch protection, a PR that touches none of the filtered paths never
  gets the check and would stay blocked. Either leave these checks non-required or
  drop the `paths:` filter.
- **Cold builds** take several minutes per OS (about 3.5 minutes release on an M2 Pro,
  from NOTES-M0); the job timeout is 45 minutes.

### Packaging job

`package` runs only on manual `workflow_dispatch` (input `package`, default true),
after `check` passes. It builds **unsigned** artifacts and uploads them:

| OS | Artifact | Built with |
| --- | --- | --- |
| macOS | `FlightDeck-<ver>-macos.zip` | `scripts/desktop/macos-bundle.sh` |
| Linux | `.deb`, `.rpm`, `.AppImage` | `cargo-deb`, `cargo-generate-rpm`, `scripts/desktop/linux-appimage.sh` |
| Windows | `FlightDeck-windows-x64.msi` | `cargo-wix` via `scripts/desktop/windows-package.ps1` |

Signing secrets are not wired into the workflow. Add them as step `env:` when the
owner has the identities (below).

## macOS (`scripts/desktop/macos-bundle.sh`)

Builds `target/desktop-dist/FlightDeck.app` and `FlightDeck-<ver>-macos.zip` and prints
the zip's sha256 (for the cask).

- Info.plist: bundle id `agency.neworange.flightdeck.desktop` (the iOS app uses the
  prefix `agency.neworange.flightdeck`, `ios/project.yml`), `LSMinimumSystemVersion`
  11.0 (`MIN_MACOS`), `NSHighResolutionCapable`, version from `desktop/Cargo.toml`.
- Icon: `AppIcon.icns` generated with `sips` + `iconutil` from
  `desktop/packaging/icons/flightdeck-1024.png`. That master is a copy of the iOS app icon
  (the repo has no other icon; `assets/` only holds a sound). It has no alpha channel
  and is a full-bleed square, so macOS shows it as a square tile; a proper
  rounded-rectangle Mac icon needs design work.
- Shaders: the script builds with the crate's default `runtime-shaders` feature, so
  the app compiles Metal shaders at each launch. On a machine with the Metal Toolchain
  ship precompiled ones: `CARGO_FEATURES_FLAGS=--no-default-features scripts/desktop/macos-bundle.sh`.
- Signing (skipped when unset): `APPLE_SIGNING_IDENTITY` runs `codesign --options runtime`
  (hardened runtime, timestamped). The app currently needs no entitlements file; add one
  if a feature requires it (the TUI's e2e entitlements are in `scripts/e2e/`).
- Notarization (skipped unless signed **and** credentials are set): either
  `NOTARY_KEYCHAIN_PROFILE` (a `xcrun notarytool store-credentials` profile), or
  `APPLE_ID` + `APPLE_TEAM_ID` + `APPLE_APP_PASSWORD`. The script submits the zip with
  `--wait`, staples the app and re-zips.
- Other env: `SKIP_BUILD=1`, `TARGET=<triple>` (cross-arch), `BUNDLE_ID`, `MIN_MACOS`.
  Universal binaries are not produced; build each arch and `lipo` if wanted.
- Homebrew cask: `packaging/homebrew/flightdeck-desktop.rb.tmpl` (placeholders
  `@VERSION@`, `@SHA256@`). The existing TUI formula is published by cargo-dist to
  `neworange-ruud/homebrew-tap`; the cask would go into `Casks/` of the same tap.
  cargo-dist does not know the GUI (`dist = false` in `desktop/Cargo.toml`), so the
  release job for the zip and the cask update is still to be written.

Local verification (2026-09-29, Darwin 27, Apple silicon, unsigned): the script built
the bundle and `plutil -lint` passed. Executing the bundle's binary from inside a git
repository ran and stayed up until killed. Launching through Finder or `open -n`
(working directory `/`) exits at once with `flightdeck error: git error: not inside a
Git repository`, because startup requires the current directory to be a git project.
That is app behaviour in `desktop/src`, not a packaging fault, but a `.app` cannot ship
until the GUI starts without a repo (project picker or last-opened project).

## Linux

Files: `desktop/packaging/linux/flightdeck-desktop.desktop`, PNG icons in
`desktop/packaging/icons/` (16 to 512 px, installed as hicolor `flightdeck-desktop.png`),
`[package.metadata.deb]` and `[package.metadata.generate-rpm]` in `desktop/Cargo.toml`,
and `scripts/desktop/linux-appimage.sh`.

- **Runtime dependencies** (declared in the deb; the rpm lists the equivalents):
  libxkbcommon (+ x11), libwayland-client, libxcb, fontconfig, freetype, a Vulkan
  loader and a driver (`mesa-vulkan-drivers` or any Vulkan ICD). GPUI renders through
  wgpu/Vulkan, so a machine without a Vulkan driver cannot run the app.
- `.deb`: `cargo deb -p flightdeck-desktop --no-build` after a release build.
- `.rpm`: `cargo generate-rpm -p desktop` from the repo root after a release build.
  Auto-detected library requirements are used besides the explicit ones.
- AppImage: `scripts/desktop/linux-appimage.sh` uses `linuxdeploy` (path via
  `$LINUXDEPLOY`). It deliberately does **not** bundle `libvulkan`/GPU drivers, because
  they must match the host. Set `APPIMAGE_EXTRACT_AND_RUN=1` where FUSE is missing.
- Untested guesses to check on first run: the deb `depends` names on Debian vs Ubuntu,
  and the Wayland `app_id` / `StartupWMClass` (the `.desktop` file has none because the
  app does not set an app id yet).

## Windows

Files: `desktop/wix/main.wxs`, `[package.metadata.wix]` in `desktop/Cargo.toml`,
`desktop/packaging/icons/flightdeck.ico`, `scripts/desktop/windows-package.ps1`.

- Installer: WiX v3 through `cargo-wix` (`cargo install cargo-wix`, WiX v3 toolset on
  PATH). Per-machine install under `Program Files\FlightDeck`, Start-menu shortcut,
  Add/Remove Programs entry with the icon, in-place major upgrades. No PATH entry.
  The `upgrade-guid` identifies the product line and must never change.
  MSIX was not chosen because it needs a signing certificate to install at all.
- Signing hook: `scripts/desktop/windows-package.ps1` runs `signtool` on the MSI when
  `WINDOWS_SIGN_PFX_PATH` (and `WINDOWS_SIGN_PFX_PASSWORD`, optional
  `WINDOWS_SIGN_TIMESTAMP_URL`) are set, and skips it otherwise. Neither the `.exe`
  inside the MSI nor the MSI itself is signed by default, so SmartScreen will warn.
- **Non-pure-Rust exception.** The TUI's Windows build is pure Rust and needs no C
  toolchain (`dist-workspace.toml` comments, `Cargo.toml` target gating). The GUI is
  not: `gpui-pre-windows` compiles HLSL shaders with `fxc.exe` and embeds a manifest
  with `rc.exe`, so building `flightdeck-desktop` on Windows requires **MSVC plus the
  Windows 10/11 SDK** (`GPUI_FXC_PATH` overrides the fxc lookup). This does not leak
  into the TUI: `default-members = ["."]` keeps `cargo build`, the TUI gate and
  cargo-dist off the GUI crate, and it cannot use the self-contained windows-gnu setup
  in `scripts/build-windows`.

## Self-update (`remote-control-bmej.6.5`)

Code: `desktop/src/selfupdate/`, the banner in `desktop/src/overlays/update.rs`.
The CLI's `flightdeck update` (axoupdater) is unchanged.

**Behaviour.** Once a day (own cache, `desktop-update-check.json` next to the TUI's,
honouring `[update] check`) the app asks GitHub for the newest `desktop-v*` release and,
if it is newer, shows the banner (never a modal). What the banner offers depends on how
the app was installed, detected from where the executable runs:

| Install | Detected by | Banner |
| --- | --- | --- |
| Homebrew cask | bundle under `/opt/homebrew/Caskroom` or `/usr/local/Caskroom`, or in an `Applications` folder with a `Caskroom/flightdeck-desktop` receipt | "Update with: brew update && brew upgrade --cask flightdeck-desktop" |
| `.deb` / `.rpm` | exe under `/usr` (not `/usr/local`) and `/var/lib/dpkg/info/flightdeck-desktop.list` / an rpm database | the apt / dnf command |
| MSI / winget | exe under `%ProgramFiles%`, `%ProgramFiles(x86)%`, `%ProgramW6432%` | winget or new MSI |
| macOS `.app` (zip), AppImage (`$APPIMAGE`), Windows portable zip | none of the above | **Update now** |
| bare binary, `/usr/local`, unknown | anything else | "download from the releases page" |

**Update now** downloads the asset and its checksum, verifies SHA-256, and only then
touches the installation. Staging is always a sibling of what it replaces (rename is
atomic only on one volume):

- macOS: unzip (`ditto -x -k`) to `.FlightDeck-update-<v>/`, rename the bundle to
  `previous.app`, rename the new `.app` into place (two renames; the first is undone if
  the second fails), delete staging, offer **Restart** (`App::restart`, which runs the
  normal quit teardown). The app needs write access to the bundle's folder; without it
  the banner shows the error and the installed copy is untouched.
- AppImage: download to `.<name>.update-<v>/`, `chmod +x`, rename over the file (atomic;
  the running image keeps its old inode), offer Restart.
- Windows portable: Windows cannot overwrite a running `.exe`, so the update is unpacked
  to `.flightdeck-update/payload` and a `READY` marker written last. At the **next
  launch**, before any window opens, `finish_staged` renames each old top-level entry
  into `.flightdeck-old/`, moves the new one in (all or nothing) and relaunches; the
  parked files are deleted on the launch after.

**Asset naming contract** (the release pipeline must publish exactly this; the updater
finds files by name and refuses, rather than guesses, when one is missing):

```text
tag       desktop-v<version>                         e.g. desktop-v1.4.0  (plain major.minor.patch)
macOS     FlightDeck-<version>-macos-<arch>.zip      FlightDeck.app at the zip root (ditto --keepParent)
Linux     FlightDeck-<version>-linux-<arch>.AppImage
Windows   FlightDeck-<version>-windows-<arch>-portable.zip   flightdeck-desktop.exe at the zip root
checksum  <asset name>.sha256                        `sha256sum` format: <64 hex>  <asset name>
```

`<arch>` is `x86_64` or `aarch64`. Drafts and pre-releases are ignored. **Gap to close in
the pipeline:** `scripts/desktop/macos-bundle.sh` currently names the zip
`FlightDeck-<ver>-macos.zip` (no arch) and no script writes `.sha256` files or builds the
portable Windows zip; those need aligning with the names above when the release job is
written. The `.msi`, `.deb`, `.rpm` and the cask are for package managers and are never
downloaded by the app.

**Downloads and unzip** use `curl` (HTTPS only, `--fail`), `ditto` (macOS), `tar` (Windows
10+) and `unzip` (Linux), by argv, so the crate carries no HTTP/TLS client or zip library.
A machine without `curl` simply never shows the notice.

**Not verified.** The release pipeline is unpublished, so the real download, the real
`ditto`/`tar` extraction, `App::restart` after a swap, writing into `/Applications`,
Gatekeeper behaviour on a swapped (unsigned or notarized) bundle, and everything on
Linux and Windows have not been run. What is tested: install-kind detection for every
OS from paths, asset selection, checksum parsing and mismatch refusal, GitHub response
parsing, the once-a-day cache, and the whole install/rollback/staged-swap logic against a
fake release source and temporary directories.

## Open questions for the owner

1. Bundle id: `agency.neworange.flightdeck.desktop` (derived from the iOS prefix)? It is
   hard to change later, because it keys the macOS preferences and Gatekeeper history.
2. Signing identities: Apple Developer ID Application cert and notary credentials, and
   a Windows code-signing certificate (EV avoids SmartScreen warnings).
3. Cask location: the existing `neworange-ruud/homebrew-tap`, and the release tag
   scheme the template assumes (`desktop-v<version>`)?
4. A proper macOS/Windows icon (the iOS icon is reused as is).
5. Should the GUI join cargo-dist releases, or keep its own release workflow?
