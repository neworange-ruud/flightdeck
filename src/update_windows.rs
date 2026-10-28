//! `flightdeck update` on Windows (SPECS §29): the same contract as the
//! axoupdater path in `src/update.rs`, without axoupdater.
//!
//! The Windows build leaves axoupdater out: its HTTP client brings rustls on
//! `aws-lc-sys`, a C library the pure-Rust windows-msvc release build avoids
//! (see Cargo.toml). All axoupdater does for us is check the latest release and
//! re-run that release's installer, so this module does the same with what
//! Windows already has: the release check goes over SChannel (the TLS the relay
//! client uses here) and the installer is cargo-dist's own
//! `flightdeck-installer.ps1`.
//!
//! A running `.exe` is locked on Windows, so the installer cannot replace the
//! binary while any FlightDeck process — this one included — still runs from
//! it. [`run`] therefore hands the install to a PowerShell helper in its own
//! console window and exits; the helper waits until the executable can be
//! opened for writing (every instance closed) and only then runs the installer.
//!
//! The receipt guard is the same as off Windows: only an install whose receipt
//! was written for this executable self-updates; anything else is told to
//! re-run the installer.

use crate::contracts::error::{FlightDeckError, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

/// App name as recorded in the install receipt and GitHub Release artifacts.
const APP_NAME: &str = "flightdeck";
/// GitHub coordinates of the published releases (mirrors `dist-workspace.toml`).
const RELEASE_OWNER: &str = "neworange-ruud";
const RELEASE_REPO: &str = "flightdeck";
/// The GitHub API host the release check asks.
const API_HOST: &str = "api.github.com";
/// How long the release check may take before `flightdeck update` gives up.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// `flightdeck update`: check GitHub Releases and, when a newer version exists,
/// start the installer in a window of its own that runs once every FlightDeck
/// process has closed. Does not launch the TUI and does not require being inside
/// a Git repository.
pub fn run() -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| FlightDeckError::Other(format!("update failed: {e}")))?;
    let Some(receipt) = read_receipt().filter(|r| receipt_is_for(r, &exe)) else {
        println!("{}", not_self_updatable_guidance());
        return Ok(());
    };

    println!("FlightDeck: checking for updates…");
    let tag = fetch_latest_tag()
        .map_err(|e| FlightDeckError::Other(format!("update check failed: {e}")))?;
    match is_newer(&tag, crate::VERSION) {
        Some(true) => {}
        Some(false) => {
            println!("FlightDeck: already up to date (v{}).", crate::VERSION);
            return Ok(());
        }
        None => {
            return Err(FlightDeckError::Other(format!(
                "update check failed: unexpected release tag {tag:?}"
            )))
        }
    }

    let latest = tag.trim_start_matches('v');
    let script = helper_script(&HelperPlan {
        exe: &exe,
        install_dir: &receipt.install_prefix,
        parent_pid: std::process::id(),
        from: crate::VERSION,
        to: latest,
        installer_url: &installer_url(&tag),
    });
    spawn_helper(&script)
        .map_err(|e| FlightDeckError::Other(format!("could not start the installer: {e}")))?;
    println!(
        "FlightDeck: updating {} → {latest} in a new window.\n\
         The installer runs once every FlightDeck process has closed; this one exits now.",
        crate::VERSION
    );
    Ok(())
}

/// The update notice (SPECS §30) is not built on Windows yet: its once-a-day
/// cache and comparison live in the axoupdater module. `flightdeck update`
/// still works; it is only the status-bar hint that never appears.
pub fn start_check(_enabled: bool, _now_unix: u64, _tx: Sender<String>) -> Option<String> {
    None
}

/// Guidance printed when this install can't be self-updated (no receipt, or a
/// receipt for another binary). Pure, so the deferral path is unit-testable.
fn not_self_updatable_guidance() -> String {
    format!(
        "FlightDeck: this install can't self-update via `{APP_NAME} update` (no install \
         receipt for this binary).\n\
         \n\
         Re-run the installer to get the latest release:\n\
         \x20\x20irm https://github.com/{RELEASE_OWNER}/{RELEASE_REPO}/releases/latest/download/\
         {APP_NAME}-installer.ps1 | iex"
    )
}

// ---------------------------------------------------------------------------
// Install receipt
// ---------------------------------------------------------------------------

/// The fields of cargo-dist's install receipt this module reads.
#[derive(Debug, Deserialize)]
struct Receipt {
    install_prefix: PathBuf,
    install_layout: String,
}

/// Where the PowerShell installer writes the receipt: `XDG_CONFIG_HOME` when
/// set, else `LOCALAPPDATA` (mirrors `flightdeck-installer.ps1`).
fn receipt_path() -> Option<PathBuf> {
    let home = std::env::var_os("XDG_CONFIG_HOME").or_else(|| std::env::var_os("LOCALAPPDATA"))?;
    Some(
        PathBuf::from(home)
            .join(APP_NAME)
            .join(format!("{APP_NAME}-receipt.json")),
    )
}

fn read_receipt() -> Option<Receipt> {
    let raw = std::fs::read_to_string(receipt_path()?).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Where a receipt says the binary lives. The PowerShell installer only writes
/// the `cargo-home` layout (`<prefix>\bin`); any other layout defers.
fn installed_exe(receipt: &Receipt) -> Option<PathBuf> {
    (receipt.install_layout == "cargo-home").then(|| {
        receipt
            .install_prefix
            .join("bin")
            .join(format!("{APP_NAME}.exe"))
    })
}

/// Whether `receipt` was written for the executable at `exe`. A receipt for a
/// different copy (a dev build, a moved binary) must not trigger an install
/// somewhere else.
fn receipt_is_for(receipt: &Receipt, exe: &Path) -> bool {
    let Some(installed) = installed_exe(receipt) else {
        return false;
    };
    match (installed.canonicalize(), exe.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Release check
// ---------------------------------------------------------------------------

/// The fields of GitHub's "latest release" answer this module reads.
#[derive(Debug, Deserialize)]
struct LatestRelease {
    tag_name: String,
}

/// `major.minor.patch` of a version or release tag (`v1.30.0`, `1.30.0`).
/// Pre-release and build suffixes are ignored: the latest release is never a
/// pre-release, and this binary's version is a plain release one.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

/// Whether the release `tag` is newer than `current`; `None` when either does
/// not parse.
fn is_newer(tag: &str, current: &str) -> Option<bool> {
    Some(parse_version(tag)? > parse_version(current)?)
}

/// The installer of exactly the release the check found, so the window installs
/// the version `flightdeck update` announced.
fn installer_url(tag: &str) -> String {
    format!(
        "https://github.com/{RELEASE_OWNER}/{RELEASE_REPO}/releases/download/{tag}/\
         {APP_NAME}-installer.ps1"
    )
}

/// Ask GitHub for the latest release's tag. Blocking: drives the request on a
/// current-thread runtime of its own, like `src/update.rs`.
fn fetch_latest_tag() -> std::result::Result<String, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime setup failed: {e}"))?;
    rt.block_on(async {
        tokio::time::timeout(CHECK_TIMEOUT, get_latest_tag())
            .await
            .map_err(|_| "GitHub did not answer in time".to_string())?
    })
}

async fn get_latest_tag() -> std::result::Result<String, String> {
    use http_body_util::{BodyExt, Empty};
    use hyper::body::Bytes;
    use hyper::header::{ACCEPT, HOST, USER_AGENT};

    let tcp = tokio::net::TcpStream::connect((API_HOST, 443))
        .await
        .map_err(|e| format!("tcp connect failed: {e}"))?;
    let io = crate::remote::push::tls_connect(API_HOST, tcp).await?;
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(io))
            .await
            .map_err(|e| format!("http handshake failed: {e}"))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    // GitHub's API rejects requests without a User-Agent.
    let request = hyper::Request::get(format!(
        "/repos/{RELEASE_OWNER}/{RELEASE_REPO}/releases/latest"
    ))
    .header(HOST, API_HOST)
    .header(USER_AGENT, format!("{APP_NAME}/{}", crate::VERSION))
    .header(ACCEPT, "application/vnd.github+json")
    .body(Empty::<Bytes>::new())
    .map_err(|e| format!("bad request: {e}"))?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|e| format!("http request failed: {e}"))?;
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("reading the answer failed: {e}"))?
        .to_bytes();
    if !status.is_success() {
        return Err(format!("GitHub answered {status}"));
    }
    let release: LatestRelease =
        serde_json::from_slice(&body).map_err(|e| format!("unexpected answer: {e}"))?;
    Ok(release.tag_name)
}

// ---------------------------------------------------------------------------
// The installer window
// ---------------------------------------------------------------------------

/// What the helper window needs to know.
struct HelperPlan<'a> {
    /// The executable to replace; the helper waits until it is free.
    exe: &'a Path,
    /// The receipt's install prefix, passed to the installer so it installs
    /// exactly where this binary lives.
    install_dir: &'a Path,
    /// This process, which holds the executable until it exits.
    parent_pid: u32,
    from: &'a str,
    to: &'a str,
    installer_url: &'a str,
}

/// A PowerShell single-quoted literal. Inside one only a quote is special, and
/// PowerShell also reads the typographic single quotes as quotes, so every one
/// of them is doubled.
fn ps_quote(s: &str) -> String {
    let mut quoted = String::with_capacity(s.len() + 2);
    quoted.push('\'');
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            quoted.push(c);
        }
        quoted.push(c);
    }
    quoted.push('\'');
    quoted
}

/// The script the helper window runs. Order matters: wait for this process,
/// then until the executable can be opened for writing with no sharing (no
/// FlightDeck runs from it any more), and only then download and run the
/// installer.
fn helper_script(plan: &HelperPlan) -> String {
    let exe = ps_quote(&plan.exe.to_string_lossy());
    let install_dir = ps_quote(&plan.install_dir.to_string_lossy());
    let from = ps_quote(plan.from);
    let to = ps_quote(plan.to);
    let url = ps_quote(plan.installer_url);
    let pid = plan.parent_pid;
    format!(
        r#"$Host.UI.RawUI.WindowTitle = 'FlightDeck update'
$exe = {exe}
$installDir = {install_dir}
$from = {from}
$to = {to}
$installerUrl = {url}

function Test-ExeFree {{
  if (-not (Test-Path -LiteralPath $exe)) {{ return $true }}
  try {{ [IO.File]::Open($exe, 'Open', 'ReadWrite', 'None').Close(); return $true }} catch {{ return $false }}
}}

function Finish($code) {{
  Write-Host ''
  Read-Host 'Press Enter to close this window' | Out-Null
  exit $code
}}

Write-Host "FlightDeck update: $from -> $to"
Wait-Process -Id {pid} -ErrorAction SilentlyContinue

$announced = $false
while (-not (Test-ExeFree)) {{
  if (-not $announced) {{
    Write-Host ''
    Write-Host "Waiting for FlightDeck to close. Still running from $exe"
    Get-Process -Name 'flightdeck' -ErrorAction SilentlyContinue |
      Where-Object {{ $_.Path -eq $exe }} |
      ForEach-Object {{ Write-Host "  process $($_.Id)" }}
    Write-Host 'The update starts as soon as every FlightDeck process (the app, workers) has closed.'
    Write-Host 'Close this window to cancel.'
    $announced = $true
  }}
  Start-Sleep -Seconds 1
}}

Write-Host ''
Write-Host "Downloading the FlightDeck $to installer..."
$installer = Join-Path ([IO.Path]::GetTempPath()) ("flightdeck-installer-" + [guid]::NewGuid() + '.ps1')
try {{
  Invoke-WebRequest -UseBasicParsing -Uri $installerUrl -OutFile $installer
}} catch {{
  Write-Host "Download failed: $_"
  Finish 1
}}

$env:FLIGHTDECK_INSTALL_DIR = $installDir
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $installer
$code = $LASTEXITCODE
Remove-Item -LiteralPath $installer -ErrorAction SilentlyContinue
if ($code -ne 0) {{
  Write-Host "The installer failed (exit code $code)."
  Finish $code
}}

Write-Host ''
Write-Host "Installed: $(& $exe --version)"
Finish 0
"#
    )
}

/// `-EncodedCommand` takes the script as base64 of its UTF-16LE bytes, which
/// sidesteps every quoting rule between here and PowerShell.
fn encode_command(script: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    STANDARD.encode(bytes)
}

/// Start the helper in a console window of its own. It goes through `start`
/// so the window gets fresh standard handles: a child spawned directly would
/// inherit this console's handles and write into the terminal that is about to
/// be handed back to the user.
fn spawn_helper(script: &str) -> std::io::Result<()> {
    use std::process::{Command, Stdio};

    Command::new("cmd.exe")
        .args([
            "/C",
            "start",
            "FlightDeck update",
            "powershell.exe",
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &encode_command(script),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other(format!(
                    "`start` exited with {status}"
                )))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(prefix: &Path, layout: &str) -> Receipt {
        Receipt {
            install_prefix: prefix.to_path_buf(),
            install_layout: layout.to_string(),
        }
    }

    #[test]
    fn reads_the_receipt_the_powershell_installer_writes() {
        let raw = r#"{"binaries":["flightdeck.exe"],"binary_aliases":{},"cdylibs":[],"cstaticlibs":[],"install_layout":"cargo-home","install_prefix":"C:\\Users\\me\\.cargo","modify_path":true,"provider":{"source":"cargo-dist","version":"0.32.0"},"source":{"app_name":"flightdeck","name":"flightdeck","owner":"neworange-ruud","release_type":"github"},"version":"1.29.0"}"#;
        let receipt: Receipt = serde_json::from_str(raw).unwrap();
        assert_eq!(
            installed_exe(&receipt),
            Some(PathBuf::from(r"C:\Users\me\.cargo\bin\flightdeck.exe"))
        );
    }

    #[test]
    fn only_a_receipt_for_this_executable_self_updates() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("flightdeck.exe");
        std::fs::write(&exe, b"").unwrap();
        let elsewhere = dir.path().join("flightdeck.exe");
        std::fs::write(&elsewhere, b"").unwrap();

        assert!(receipt_is_for(&receipt(dir.path(), "cargo-home"), &exe));
        assert!(
            !receipt_is_for(&receipt(dir.path(), "cargo-home"), &elsewhere),
            "a receipt for another copy must defer"
        );
        assert!(
            !receipt_is_for(&receipt(dir.path(), "flat"), &exe),
            "a layout the installer does not write must defer"
        );
        assert!(
            !receipt_is_for(&receipt(&dir.path().join("gone"), "cargo-home"), &exe),
            "a receipt whose binary is missing must defer"
        );
    }

    #[test]
    fn guidance_points_at_the_powershell_installer() {
        let msg = not_self_updatable_guidance();
        assert!(msg.contains("releases/latest/download/flightdeck-installer.ps1 | iex"));
        assert!(
            !msg.to_lowercase().contains("updated"),
            "guidance must not imply a self-update happened: {msg}"
        );
    }

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v1.30.0"), Some((1, 30, 0)));
        assert_eq!(parse_version("1.30.0-beta.1"), Some((1, 30, 0)));
        assert_eq!(parse_version("desktop-v0.10.0"), None);
        assert_eq!(parse_version("1.30"), None);
        assert_eq!(parse_version("1.30.0.1"), None);

        assert_eq!(is_newer("v1.30.0", "1.29.0"), Some(true));
        assert_eq!(
            is_newer("v1.10.0", "1.9.9"),
            Some(true),
            "not a string compare"
        );
        assert_eq!(is_newer("v1.29.0", "1.29.0"), Some(false));
        assert_eq!(is_newer("v1.28.0", "1.29.0"), Some(false));
        assert_eq!(is_newer("nightly", "1.29.0"), None);
    }

    #[test]
    fn the_installer_url_names_the_checked_release() {
        assert_eq!(
            installer_url("v1.30.0"),
            "https://github.com/neworange-ruud/flightdeck/releases/download/v1.30.0/flightdeck-installer.ps1"
        );
    }

    #[test]
    fn ps_quote_doubles_every_single_quote() {
        assert_eq!(ps_quote(r"C:\a b\c"), r"'C:\a b\c'");
        assert_eq!(ps_quote("it's"), "'it''s'");
        assert_eq!(ps_quote("it\u{2019}s"), "'it\u{2019}\u{2019}s'");
        assert_eq!(
            ps_quote("$env:X"),
            "'$env:X'",
            "no expansion inside single quotes"
        );
    }

    fn plan_script() -> String {
        helper_script(&HelperPlan {
            exe: Path::new(r"C:\Users\o'neil\.cargo\bin\flightdeck.exe"),
            install_dir: Path::new(r"C:\Users\o'neil\.cargo"),
            parent_pid: 4242,
            from: "1.29.0",
            to: "1.30.0",
            installer_url: &installer_url("v1.30.0"),
        })
    }

    #[test]
    fn the_helper_waits_until_the_executable_is_free_before_installing() {
        let script = plan_script();
        let wait_parent = script
            .find("Wait-Process -Id 4242")
            .expect("waits for this process");
        let wait_free = script
            .find("while (-not (Test-ExeFree))")
            .expect("waits until the executable is free");
        let install = script
            .find("& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $installer")
            .expect("runs the installer");
        assert!(wait_parent < wait_free && wait_free < install, "{script}");
        assert!(
            script.contains("[IO.File]::Open($exe, 'Open', 'ReadWrite', 'None')"),
            "free means openable for writing with no sharing"
        );
    }

    #[test]
    fn the_helper_installs_the_checked_version_where_this_binary_lives() {
        let script = plan_script();
        assert!(script.contains(r"$exe = 'C:\Users\o''neil\.cargo\bin\flightdeck.exe'"));
        assert!(script.contains(r"$installDir = 'C:\Users\o''neil\.cargo'"));
        assert!(script.contains("$env:FLIGHTDECK_INSTALL_DIR = $installDir"));
        assert!(script.contains("releases/download/v1.30.0/flightdeck-installer.ps1"));
    }

    #[test]
    fn encode_command_is_base64_of_utf16le() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;

        let decoded = STANDARD.decode(encode_command("Write-Host 'é'")).unwrap();
        let units: Vec<u16> = decoded
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "Write-Host 'é'");
    }

    #[test]
    fn start_check_is_a_noop() {
        let (tx, _rx) = std::sync::mpsc::channel();
        assert_eq!(start_check(true, 5_000_000, tx), None);
    }
}
