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

/// The script the elevated window runs: add the exclusion, say what happened,
/// wait for a key. Single quotes only, so it nests inside another layer.
fn elevated_script(exe: &Path) -> String {
    let exe = ps_single_quote_escape(&exe.to_string_lossy());
    format!(
        "try {{ Add-MpPreference -ExclusionProcess '{exe}' -ErrorAction Stop; \
         Write-Host 'FlightDeck was added to the Windows Defender exclusions.' \
         -ForegroundColor Green }} \
         catch {{ Write-Host ('Could not add the exclusion: ' + $_.Exception.Message) \
         -ForegroundColor Red }}; \
         Write-Host 'Press any key to close this window.'; \
         [void][System.Console]::ReadKey($true)"
    )
}

/// The arguments for `powershell.exe` that open a new, elevated PowerShell
/// window running [`elevated_script`] for `exe`.
///
/// The launcher is not elevated: it only runs `Start-Process -Verb RunAs`,
/// which raises the UAC prompt and gives the elevated process a console of its
/// own. The script is inline (`-Command`), not a file, and no execution policy
/// is touched. Quoting: the script holds only single quotes; it is wrapped in
/// double quotes for the child's command line, and that whole argument string
/// sits in a single-quoted PowerShell literal, where each `'` is doubled.
pub fn launcher_args(exe: &Path) -> Vec<String> {
    let child_args = format!("-NoProfile -Command \"{}\"", elevated_script(exe));
    let outer = format!(
        "Start-Process -FilePath powershell.exe -Verb RunAs -ArgumentList '{}'",
        child_args.replace('\'', "''")
    );
    vec![
        "-NoProfile".to_string(),
        "-NonInteractive".to_string(),
        "-WindowStyle".to_string(),
        "Hidden".to_string(),
        "-Command".to_string(),
        outer,
    ]
}

/// Start the elevated window for the running executable. Fire-and-forget.
pub fn spawn_elevated_exclusion(runner: &dyn CommandRunner) -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| {
        FlightDeckError::Io(format!("could not find the FlightDeck executable: {e}"))
    })?;
    runner.spawn_detached("powershell.exe", &launcher_args(&exe))
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

    #[test]
    fn launcher_runs_an_inline_elevated_command_without_a_script_or_bypass() {
        let args = launcher_args(Path::new(r"C:\Tools\flightdeck.exe"));
        let joined = args.join(" ");
        assert_eq!(args[0], "-NoProfile");
        assert!(args.contains(&"-Command".to_string()));
        assert!(joined.contains("Start-Process -FilePath powershell.exe -Verb RunAs"));
        assert!(joined.contains(r"Add-MpPreference -ExclusionProcess ''C:\Tools\flightdeck.exe''"));
        assert!(joined.contains("ReadKey"));
        assert!(!joined.contains("-ExecutionPolicy"));
        assert!(!joined.contains("-File "));
    }

    #[test]
    fn a_path_with_spaces_and_a_quote_is_quoted_for_every_layer() {
        let exe = PathBuf::from(r"C:\Users\O'Brien\Program Files\flightdeck.exe");
        let script = elevated_script(&exe);
        // Layer 1: inside the script's own single-quoted literal.
        assert!(script.contains(r"'C:\Users\O''Brien\Program Files\flightdeck.exe'"));
        assert!(!script.contains('"'));
        // Layer 2: the outer literal doubles every quote again.
        let outer = launcher_args(&exe).pop().unwrap();
        assert!(outer.contains(r"''C:\Users\O''''Brien\Program Files\flightdeck.exe''"));
    }

    #[test]
    fn typographic_quotes_are_doubled_too() {
        assert_eq!(ps_single_quote_escape("a\u{2019}b"), "a\u{2019}\u{2019}b");
    }

    #[test]
    fn spawning_goes_through_the_runner() {
        let runner = FakeCommandRunner::new();
        spawn_elevated_exclusion(&runner).unwrap();
        let spawned = runner.spawned();
        assert_eq!(spawned.len(), 1);
        assert_eq!(spawned[0].0, "powershell.exe");
        let exe = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let escaped = ps_single_quote_escape(&exe).replace('\'', "''");
        assert!(spawned[0].1.last().unwrap().contains(&escaped));
    }

    #[test]
    fn a_failed_spawn_is_an_error_not_a_panic() {
        let runner = FakeCommandRunner::new();
        runner.fail_to_launch("no powershell");
        assert!(spawn_elevated_exclusion(&runner).is_err());
    }
}
