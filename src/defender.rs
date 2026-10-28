//! The Windows Defender exclusion offer.
//!
//! On Windows, Defender's real-time scan slows FlightDeck down, so on startup
//! FlightDeck offers once to add itself to Defender's process exclusions. The
//! decision logic here is platform-independent (callers pass "is Windows" and
//! "is isolated" in); only the trigger in the TUI is gated on the platform.
//! Persistence goes through [`FileSystem`]; the elevated window is started
//! through [`CommandRunner::spawn_detached`], so both are fakeable.

use crate::contracts::{CommandRunner, FileSystem, FlightDeckError, Result};
use crate::persistence::global_state::{
    global_state_path, load_global_state, save_global_state, DefenderExclusionChoice, GlobalState,
};
use std::path::Path;

/// The question the popup asks.
pub const QUESTION: &str = "You are on a Windows machine. Would you like to add FlightDeck to \
                            your Defender exclusions?";

/// Whether the offer is due: only on Windows, never in an isolated run, never
/// after an acceptance, and not again for a version the user declined it on.
/// A missing record (a fresh install) counts as a new version.
pub fn should_prompt(
    state: &GlobalState,
    is_windows: bool,
    isolated: bool,
    current_version: &str,
) -> bool {
    if !is_windows || isolated {
        return false;
    }
    match &state.defender_exclusion {
        None => true,
        Some(DefenderExclusionChoice::Accepted) => false,
        Some(DefenderExclusionChoice::Declined { version }) => version != current_version,
    }
}

/// Remember the answer, keeping the rest of the global state. Best-effort:
/// with no home directory there is nowhere to write and nothing happens.
pub fn record_choice(fs: &dyn FileSystem, choice: DefenderExclusionChoice) {
    let Some(path) = global_state_path() else {
        return;
    };
    let mut state = load_global_state(fs, &path);
    state.defender_exclusion = Some(choice);
    let _ = save_global_state(fs, &path, &state);
}

/// Whether the offer is due now, read from the real global state location.
pub fn due(fs: &dyn FileSystem, is_windows: bool, isolated: bool, current_version: &str) -> bool {
    let state = global_state_path()
        .map(|path| load_global_state(fs, &path))
        .unwrap_or_default();
    should_prompt(&state, is_windows, isolated, current_version)
}

/// Double every character PowerShell treats as a single quote (the ASCII one
/// and the four typographic ones), so `s` is safe inside `'...'`.
fn ps_single_quote_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        out.push(c);
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
    }
    out
}

/// `s` as a PowerShell single-quoted string literal.
fn ps_literal(s: &str) -> String {
    format!("'{}'", ps_single_quote_escape(s))
}

/// The script the elevated window runs: add the exclusion, say what happened,
/// wait for a key.
fn elevated_script(exe: &Path) -> String {
    let exe = ps_literal(&exe.to_string_lossy());
    format!(
        "try {{ Add-MpPreference -ExclusionProcess {exe} -ErrorAction Stop; \
         Write-Host 'FlightDeck was added to the Windows Defender exclusions.' \
         -ForegroundColor Green }} \
         catch {{ Write-Host ('Could not add the exclusion: ' + $_.Exception.Message) \
         -ForegroundColor Red }}; \
         Write-Host 'Press any key to close this window.'; \
         [void][System.Console]::ReadKey($true)"
    )
}

/// What `-EncodedCommand` takes: the script as UTF-16LE, base64. The encoded
/// text holds no quote or space, so no layer of quoting can mangle it.
fn encode_command(script: &str) -> String {
    use base64::Engine;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The absolute path of Windows PowerShell, so a `powershell.exe` planted in
/// the working directory or earlier on `PATH` is never the one that runs
/// elevated. Under `%SystemRoot%`, falling back to `C:\Windows`.
fn powershell_path() -> String {
    let root = std::env::var("SystemRoot")
        .ok()
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| r"C:\Windows".to_string());
    format!(
        r"{}\System32\WindowsPowerShell\v1.0\powershell.exe",
        root.trim_end_matches(['\\', '/'])
    )
}

/// The arguments for the PowerShell at `powershell` that open a new, elevated
/// PowerShell window running [`elevated_script`] for `exe`.
///
/// The launcher is not elevated: it only runs `Start-Process -Verb RunAs`,
/// which raises the UAC prompt and gives the elevated process a console of its
/// own. Both scripts travel as `-EncodedCommand`, not as quoted text, so the
/// only quoting left is the executable's path inside a single PowerShell
/// literal. No script file is written and no execution policy is touched.
pub fn launcher_args(exe: &Path, powershell: &str) -> Vec<String> {
    let outer = format!(
        "Start-Process -FilePath {} -Verb RunAs -ArgumentList \
         '-NoProfile','-EncodedCommand','{}'",
        ps_literal(powershell),
        encode_command(&elevated_script(exe)),
    );
    vec![
        "-NoProfile".to_string(),
        "-NonInteractive".to_string(),
        "-WindowStyle".to_string(),
        "Hidden".to_string(),
        "-EncodedCommand".to_string(),
        encode_command(&outer),
    ]
}

/// Start the elevated window for the running executable. Fire-and-forget.
pub fn spawn_elevated_exclusion(runner: &dyn CommandRunner) -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| {
        FlightDeckError::Io(format!("could not find the FlightDeck executable: {e}"))
    })?;
    let powershell = powershell_path();
    runner.spawn_detached(&powershell, &launcher_args(&exe, &powershell))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeCommandRunner, FakeFs};
    use std::path::PathBuf;

    const V: &str = "1.30.0";

    fn declined(v: &str) -> GlobalState {
        GlobalState {
            defender_exclusion: Some(DefenderExclusionChoice::Declined {
                version: v.to_string(),
            }),
            ..GlobalState::default()
        }
    }

    #[test]
    fn never_recorded_shows() {
        assert!(should_prompt(&GlobalState::default(), true, false, V));
    }

    #[test]
    fn accepted_never_shows_again() {
        let state = GlobalState {
            defender_exclusion: Some(DefenderExclusionChoice::Accepted),
            ..GlobalState::default()
        };
        assert!(!should_prompt(&state, true, false, V));
    }

    #[test]
    fn declined_for_this_version_stays_quiet() {
        assert!(!should_prompt(&declined(V), true, false, V));
    }

    #[test]
    fn declined_for_an_older_version_shows_again() {
        assert!(should_prompt(&declined("1.29.0"), true, false, V));
    }

    #[test]
    fn not_windows_never_shows() {
        assert!(!should_prompt(&GlobalState::default(), false, false, V));
    }

    #[test]
    fn isolated_never_shows() {
        assert!(!should_prompt(&GlobalState::default(), true, true, V));
    }

    #[test]
    fn recording_replaces_the_answer_in_the_global_file() {
        let fs = FakeFs::new();
        let Some(path) = global_state_path() else {
            return; // no home directory in this environment
        };
        record_choice(
            &fs,
            DefenderExclusionChoice::Declined {
                version: V.to_string(),
            },
        );
        assert!(!due(&fs, true, false, V));
        assert!(due(&fs, true, false, "1.31.0"));
        record_choice(&fs, DefenderExclusionChoice::Accepted);
        assert!(!due(&fs, true, false, "9.9.9"));
        let raw = fs.file_contents(&path).unwrap();
        assert!(raw.contains("accepted"), "{raw}");
    }

    fn decode(b64: &str) -> String {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).unwrap()
    }

    const PS: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    const MARK: &str = "'-EncodedCommand','";

    /// The base64 of the elevated script inside a launcher's outer script.
    fn inner_b64(outer: &str) -> &str {
        outer.split(MARK).nth(1).unwrap().trim_end_matches('\'')
    }

    /// The two scripts behind a launcher: (outer, elevated inner).
    fn scripts(args: &[String]) -> (String, String) {
        let outer = decode(args.last().unwrap());
        let inner = decode(inner_b64(&outer));
        (outer, inner)
    }

    #[test]
    fn the_launcher_runs_encoded_commands_without_a_script_file_or_bypass() {
        let args = launcher_args(Path::new(r"C:\Tools\flightdeck.exe"), PS);
        assert_eq!(
            args[..5],
            [
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-EncodedCommand"
            ]
        );
        assert_eq!(args.len(), 6);
        let (outer, inner) = scripts(&args);
        assert!(outer.starts_with(&format!("Start-Process -FilePath '{PS}' -Verb RunAs ")));
        assert!(inner.contains(r"Add-MpPreference -ExclusionProcess 'C:\Tools\flightdeck.exe' "));
        assert!(inner.contains("ReadKey"));
        for text in [&outer, &inner] {
            assert!(!text.contains("-ExecutionPolicy"));
            assert!(!text.contains("-File "));
        }
    }

    #[test]
    fn a_path_with_spaces_and_quotes_is_one_escaped_literal() {
        let exe =
            PathBuf::from("C:\\Users\\D\u{2019}Angelo\\O'Brien\\Program Files\\flightdeck.exe");
        let (_, inner) = scripts(&launcher_args(&exe, PS));
        assert!(
            inner.contains(
                "-ExclusionProcess 'C:\\Users\\D\u{2019}\u{2019}Angelo\\O''Brien\\Program Files\\flightdeck.exe' "
            ),
            "{inner}"
        );
    }

    #[test]
    fn the_encoded_arguments_hold_no_quote_or_space() {
        let exe = PathBuf::from("C:\\O'Brien \u{2018}x\u{201B}\\flightdeck.exe");
        let args = launcher_args(&exe, PS);
        let outer = decode(args.last().unwrap());
        for arg in [args.last().unwrap().as_str(), inner_b64(&outer)] {
            assert!(arg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')));
        }
    }

    #[test]
    fn every_typographic_quote_is_doubled() {
        for q in ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'] {
            assert_eq!(ps_literal(&format!("a{q}b")), format!("'a{q}{q}b'"));
        }
    }

    #[test]
    fn the_command_is_utf16le_base64() {
        // "A" is 0x41 0x00; a char outside the BMP takes a surrogate pair.
        assert_eq!(encode_command("A"), "QQA=");
        assert_eq!(decode(&encode_command("\u{1F680}x")), "\u{1F680}x");
    }

    #[test]
    fn spawning_goes_through_the_runner_with_the_absolute_powershell() {
        let runner = FakeCommandRunner::new();
        spawn_elevated_exclusion(&runner).unwrap();
        let spawned = runner.spawned();
        assert_eq!(spawned.len(), 1);
        assert_eq!(spawned[0].0, powershell_path());
        assert!(spawned[0]
            .0
            .ends_with(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
        let (outer, inner) = scripts(&spawned[0].1);
        assert!(outer.contains(&ps_literal(&powershell_path())));
        let exe = std::env::current_exe().unwrap();
        assert!(inner.contains(&ps_literal(&exe.to_string_lossy())));
    }

    #[test]
    fn a_failed_spawn_is_an_error_not_a_panic() {
        let runner = FakeCommandRunner::new();
        runner.fail_to_launch("no powershell");
        assert!(spawn_elevated_exclusion(&runner).is_err());
    }
}
