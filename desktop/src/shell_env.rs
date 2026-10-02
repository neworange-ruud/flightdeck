//! The user's login-shell environment, for a launch that did not come from a
//! terminal.
//!
//! Started from Finder, the Dock, Spotlight or a Linux desktop launcher, the
//! app gets the session manager's environment, not the shell's: on macOS
//! `PATH` is `/usr/bin:/bin:/usr/sbin:/sbin`. Every child FlightDeck starts —
//! `git`, the agents (`claude` in `~/.local/bin`, `codex` from npm, anything
//! Homebrew installed), shells, `podman` — would then resolve against that
//! `PATH` instead of the one the user's terminal has, so `git` is Apple's
//! `xcrun` shim (which fails whenever Xcode is mid-update or its licence wants
//! accepting) and most agents are simply "not found".
//!
//! So, like other desktop developer tools, the app asks the user's own shell
//! once at start-up, as a login, interactive shell (where version managers and
//! `PATH` edits live), for its environment and adopts it. Started from a
//! terminal the environment is already right, and nothing happens.
//!
//! Best effort and bounded: a shell that fails, prints garbage or takes longer
//! than [`TIMEOUT`] leaves the environment as it was, and the app starts
//! anyway. Windows needs none of this (Explorer hands a GUI app the user's
//! environment) and does nothing.

use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

/// How long the shell may take to print its environment.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Variables that describe the probing shell itself, not the user's
/// environment, and are never adopted.
const SHELL_LOCAL: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

/// The environment variable that tells the probing shell where to write.
const OUT_VAR: &str = "FLIGHTDECK_SHELL_ENV_OUT";

/// Adopt the login shell's environment when the app was not started from a
/// terminal. Call first thing in `main`, before any thread exists: it changes
/// the process environment.
#[cfg(unix)]
pub fn import_if_launched_outside_a_terminal() {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return;
    }
    let shell = user_shell();
    match capture(&shell, TIMEOUT) {
        Ok(vars) => {
            for (key, value) in adoptable(vars) {
                std::env::set_var(key, value);
            }
        }
        Err(e) => eprintln!(
            "flightdeck-desktop: could not read the environment of {}: {e}; \
             keeping the launch environment",
            shell.display()
        ),
    }
}

/// No-op: a Windows GUI app already gets the user's environment.
#[cfg(not(unix))]
pub fn import_if_launched_outside_a_terminal() {}

/// `$SHELL`, else `/bin/sh`.
#[cfg(unix)]
fn user_shell() -> std::path::PathBuf {
    std::env::var_os("SHELL")
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/bin/sh".into())
}

/// Run `shell` as a login, interactive shell and return the environment it
/// ends up with.
///
/// The shell writes `env -0` to a temporary file rather than to a pipe: an rc
/// file that starts a daemon (an ssh or gpg agent) leaves it holding the
/// shell's stdout, and a pipe would then never close. Stdin, stdout and stderr
/// are all `/dev/null`, so nothing the rc files print can get in the way.
pub fn capture(shell: &Path, timeout: Duration) -> Result<Vec<(OsString, OsString)>, String> {
    use std::process::{Command, Stdio};
    use std::time::Instant;

    static PROBES: AtomicU32 = AtomicU32::new(0);
    let out = std::env::temp_dir().join(format!(
        "flightdeck-shell-env-{}-{}",
        std::process::id(),
        PROBES.fetch_add(1, Ordering::Relaxed)
    ));
    let mut command = Command::new(shell);
    command
        .args([
            "-l",
            "-i",
            "-c",
            &format!("command env -0 > \"${OUT_VAR}\""),
        ])
        .env(OUT_VAR, &out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(home) = std::env::var_os("HOME") {
        command.current_dir(home);
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&out);
                return Err(format!("no answer within {}s", timeout.as_secs()));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&out);
                return Err(e.to_string());
            }
        }
    };
    let bytes = std::fs::read(&out);
    let _ = std::fs::remove_file(&out);
    if !status.success() {
        return Err(format!("the shell exited with {status}"));
    }
    let vars = parse(&bytes.map_err(|e| e.to_string())?);
    if vars.is_empty() {
        return Err("the shell printed no environment".to_string());
    }
    Ok(vars)
}

/// `env -0` output: `KEY=value` entries, each ended by a NUL. Values may hold
/// newlines and `=`; an entry without `=` or with an empty key is skipped.
pub fn parse(bytes: &[u8]) -> Vec<(OsString, OsString)> {
    bytes
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let eq = entry.iter().position(|b| *b == b'=')?;
            let (key, value) = (&entry[..eq], &entry[eq + 1..]);
            (!key.is_empty()).then(|| (os(key), os(value)))
        })
        .collect()
}

/// The captured variables worth adopting: everything but the probing shell's
/// own bookkeeping ([`SHELL_LOCAL`]) and the file it wrote to.
pub fn adoptable(vars: Vec<(OsString, OsString)>) -> Vec<(OsString, OsString)> {
    vars.into_iter()
        .filter(|(key, _)| {
            key.to_str()
                .is_none_or(|k| k != OUT_VAR && !SHELL_LOCAL.contains(&k))
        })
        .collect()
}

#[cfg(unix)]
fn os(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(bytes).to_os_string()
}

#[cfg(not(unix))]
fn os(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        list.iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    #[test]
    fn parse_reads_nul_separated_entries_with_newlines_and_equals_in_values() {
        let out = b"PATH=/opt/homebrew/bin:/usr/bin\0MULTI=one\ntwo\0EQ=a=b\0";
        assert_eq!(
            parse(out),
            pairs(&[
                ("PATH", "/opt/homebrew/bin:/usr/bin"),
                ("MULTI", "one\ntwo"),
                ("EQ", "a=b"),
            ])
        );
    }

    #[test]
    fn parse_skips_entries_without_a_key() {
        assert_eq!(parse(b"garbage\0=nokey\0EMPTY=\0"), pairs(&[("EMPTY", "")]));
    }

    #[test]
    fn the_probing_shell_s_own_bookkeeping_is_not_adopted() {
        let vars = pairs(&[
            ("PATH", "/x"),
            ("PWD", "/home/u"),
            ("OLDPWD", "/"),
            ("SHLVL", "2"),
            ("_", "/usr/bin/env"),
            (OUT_VAR, "/tmp/f"),
            ("NVM_DIR", "/home/u/.nvm"),
        ]);
        assert_eq!(
            adoptable(vars),
            pairs(&[("PATH", "/x"), ("NVM_DIR", "/home/u/.nvm")])
        );
    }

    /// A stand-in "shell": a script that ignores its arguments and runs
    /// `body` with the output file in `$FLIGHTDECK_SHELL_ENV_OUT`.
    #[cfg(unix)]
    fn fake_shell(dir: &Path, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-shell");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn capture_reads_what_the_shell_wrote() {
        let dir = tempfile::TempDir::new().unwrap();
        let shell = fake_shell(
            dir.path(),
            r#"printf 'PATH=/from/rc:/usr/bin\0EDITOR=vim\0' > "$FLIGHTDECK_SHELL_ENV_OUT""#,
        );
        let vars = capture(&shell, Duration::from_secs(5)).unwrap();
        assert_eq!(
            vars,
            pairs(&[("PATH", "/from/rc:/usr/bin"), ("EDITOR", "vim")])
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_real_login_shell_reports_a_path() {
        let vars = capture(Path::new("/bin/sh"), Duration::from_secs(10)).unwrap();
        assert!(vars.iter().any(|(k, v)| k == "PATH" && !v.is_empty()));
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_that_fails_changes_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let shell = fake_shell(dir.path(), "exit 3");
        assert!(capture(&shell, Duration::from_secs(5)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_that_hangs_is_abandoned_at_the_timeout() {
        let dir = tempfile::TempDir::new().unwrap();
        let shell = fake_shell(dir.path(), "exec sleep 30");
        let started = std::time::Instant::now();
        let err = capture(&shell, Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("no answer"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_missing_shell_is_an_error_not_a_panic() {
        assert!(capture(Path::new("/nonexistent/shell"), Duration::from_secs(1)).is_err());
    }
}
