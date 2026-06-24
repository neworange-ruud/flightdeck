//! Output→status classification and status combination (SPECS §24).

use crate::contracts::{InterpretedStatus, ManualStatus, ProcessState, StatusPatterns};

/// Classify agent output against the config `status_patterns` via substring
/// match (SPECS §24). Returns the matched interpreted status, if any.
///
/// Precedence (highest first): ERROR → Failed, WAITING → WaitingForInput,
/// COMPLETED → Completed. Case-sensitive match against literal patterns.
pub fn classify_output(patterns: &StatusPatterns, output: &str) -> Option<InterpretedStatus> {
    // Check ERROR patterns first (highest precedence).
    for pat in &patterns.error {
        if output.contains(pat.as_str()) {
            return Some(InterpretedStatus::Failed);
        }
    }
    // Check WAITING patterns next.
    for pat in &patterns.waiting {
        if output.contains(pat.as_str()) {
            return Some(InterpretedStatus::WaitingForInput);
        }
    }
    // Check COMPLETED patterns last.
    for pat in &patterns.completed {
        if output.contains(pat.as_str()) {
            return Some(InterpretedStatus::Completed);
        }
    }
    None
}

/// Quiet period after which a *running* agent that produced no output is
/// considered idle rather than working (SPECS §24). Agents (OpenCode, Claude
/// Code, Codex) stream output continuously while working and fall silent at
/// their prompt, so a short silence is a reliable "turn finished" signal.
pub const IDLE_AFTER_MS: u64 = 1500;

/// Compute the effective interpreted status of a **running** agent from output
/// activity timing plus the latest classified/hook signal (SPECS §24).
///
/// Precedence:
/// 1. A *sticky* signal (waiting / needs-attention / completed / failed /
///    recovered) holds until fresh output arrives **after** it — i.e. the agent
///    resumed working — at which point it is superseded.
/// 2. Otherwise activity decides: output within [`IDLE_AFTER_MS`] → `Working`,
///    older → `Idle`.
/// 3. Before any output has been seen, the raw signal (e.g. `Starting`) shows.
pub fn running_status(
    signal: Option<InterpretedStatus>,
    signal_at_ms: Option<u64>,
    last_activity_ms: Option<u64>,
    now_ms: u64,
) -> InterpretedStatus {
    if let Some(sig) = signal {
        let sticky = matches!(
            sig,
            InterpretedStatus::WaitingForInput
                | InterpretedStatus::NeedsAttention
                | InterpretedStatus::Completed
                | InterpretedStatus::Failed
                | InterpretedStatus::Recovered
        );
        if sticky {
            let superseded =
                matches!((last_activity_ms, signal_at_ms), (Some(a), Some(s)) if a > s);
            if !superseded {
                return sig;
            }
        }
    }
    match last_activity_ms {
        None => signal.unwrap_or(InterpretedStatus::Running),
        Some(t) if now_ms.saturating_sub(t) < IDLE_AFTER_MS => InterpretedStatus::Working,
        Some(_) => InterpretedStatus::Idle,
    }
}

/// The combined, display-ready status (SPECS §24). Manual override takes visual
/// priority but does not hide the process state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayStatus {
    pub process: ProcessState,
    pub interpreted: InterpretedStatus,
    pub manual: Option<ManualStatus>,
}

/// Combine process state, interpreted status, and manual override (SPECS §24).
///
/// When `interpreted` is `None`, a sensible default is derived from `process`:
/// - Running     → Running
/// - Starting    → Starting
/// - Exited(0)   → Completed
/// - Exited(n≠0) → Failed
/// - Failed      → Failed
/// - Stopped     → Stopped
/// - Lost        → SessionLost
/// - NotStarted  → Unknown
///
/// The `manual` override is stored separately so the UI can show both fields
/// (manual takes visual priority but does not hide process state, SPECS §24).
pub fn combine_status(
    process: ProcessState,
    interpreted: Option<InterpretedStatus>,
    manual: Option<ManualStatus>,
) -> DisplayStatus {
    let derived = interpreted.unwrap_or(match process {
        ProcessState::Running => InterpretedStatus::Running,
        ProcessState::Starting => InterpretedStatus::Starting,
        ProcessState::Exited(0) => InterpretedStatus::Completed,
        ProcessState::Exited(_) => InterpretedStatus::Failed,
        ProcessState::Failed => InterpretedStatus::Failed,
        ProcessState::Stopped => InterpretedStatus::Stopped,
        ProcessState::Lost => InterpretedStatus::SessionLost,
        ProcessState::NotStarted => InterpretedStatus::Unknown,
    });

    DisplayStatus {
        process,
        interpreted: derived,
        manual,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::StatusPatterns;

    fn patterns() -> StatusPatterns {
        StatusPatterns {
            waiting: vec!["waiting for input".to_string(), "> ".to_string()],
            completed: vec!["Task completed".to_string(), "Done.".to_string()],
            error: vec!["ERROR:".to_string(), "fatal error".to_string()],
        }
    }

    // -------------------------------------------------------------------------
    // classify_output tests
    // -------------------------------------------------------------------------

    #[test]
    fn classify_output_detects_waiting_pattern() {
        let p = patterns();
        assert_eq!(
            classify_output(&p, "Agent is waiting for input from user"),
            Some(InterpretedStatus::WaitingForInput)
        );
    }

    #[test]
    fn classify_output_detects_completed_pattern() {
        let p = patterns();
        assert_eq!(
            classify_output(&p, "Task completed successfully"),
            Some(InterpretedStatus::Completed)
        );
    }

    #[test]
    fn classify_output_detects_error_pattern() {
        let p = patterns();
        assert_eq!(
            classify_output(&p, "ERROR: something went wrong"),
            Some(InterpretedStatus::Failed)
        );
    }

    #[test]
    fn classify_output_error_takes_precedence_over_waiting() {
        let p = patterns();
        // Output that matches both ERROR and WAITING — ERROR wins.
        let output = "ERROR: waiting for input";
        assert_eq!(classify_output(&p, output), Some(InterpretedStatus::Failed));
    }

    #[test]
    fn classify_output_error_takes_precedence_over_completed() {
        let p = patterns();
        let output = "ERROR: Task completed but then crashed";
        assert_eq!(classify_output(&p, output), Some(InterpretedStatus::Failed));
    }

    #[test]
    fn classify_output_waiting_takes_precedence_over_completed() {
        let p = patterns();
        let output = "waiting for input — Done.";
        assert_eq!(
            classify_output(&p, output),
            Some(InterpretedStatus::WaitingForInput)
        );
    }

    #[test]
    fn classify_output_returns_none_when_no_pattern_matches() {
        let p = patterns();
        assert_eq!(classify_output(&p, "Normal log output line"), None);
    }

    #[test]
    fn classify_output_is_case_sensitive() {
        let p = patterns();
        // "error:" (lowercase) should NOT match "ERROR:".
        assert_eq!(classify_output(&p, "error: something bad"), None);
        // "task completed" (lowercase) should NOT match "Task completed".
        assert_eq!(classify_output(&p, "task completed"), None);
    }

    #[test]
    fn classify_output_empty_patterns_returns_none() {
        let empty = StatusPatterns::default();
        assert_eq!(classify_output(&empty, "ERROR: something"), None);
        assert_eq!(classify_output(&empty, "Done."), None);
    }

    // -------------------------------------------------------------------------
    // combine_status tests
    // -------------------------------------------------------------------------

    #[test]
    fn combine_status_uses_provided_interpreted_when_present() {
        let ds = combine_status(
            ProcessState::Running,
            Some(InterpretedStatus::WaitingForInput),
            None,
        );
        assert_eq!(ds.interpreted, InterpretedStatus::WaitingForInput);
        assert_eq!(ds.process, ProcessState::Running);
        assert_eq!(ds.manual, None);
    }

    #[test]
    fn combine_status_derives_running_from_process_running() {
        let ds = combine_status(ProcessState::Running, None, None);
        assert_eq!(ds.interpreted, InterpretedStatus::Running);
    }

    #[test]
    fn combine_status_derives_starting_from_process_starting() {
        let ds = combine_status(ProcessState::Starting, None, None);
        assert_eq!(ds.interpreted, InterpretedStatus::Starting);
    }

    #[test]
    fn combine_status_derives_completed_from_exited_zero() {
        let ds = combine_status(ProcessState::Exited(0), None, None);
        assert_eq!(ds.interpreted, InterpretedStatus::Completed);
    }

    #[test]
    fn combine_status_derives_failed_from_exited_nonzero() {
        let ds = combine_status(ProcessState::Exited(1), None, None);
        assert_eq!(ds.interpreted, InterpretedStatus::Failed);
        let ds2 = combine_status(ProcessState::Exited(-1), None, None);
        assert_eq!(ds2.interpreted, InterpretedStatus::Failed);
    }

    #[test]
    fn combine_status_derives_failed_from_process_failed() {
        let ds = combine_status(ProcessState::Failed, None, None);
        assert_eq!(ds.interpreted, InterpretedStatus::Failed);
    }

    #[test]
    fn combine_status_derives_stopped_from_process_stopped() {
        let ds = combine_status(ProcessState::Stopped, None, None);
        assert_eq!(ds.interpreted, InterpretedStatus::Stopped);
    }

    #[test]
    fn combine_status_derives_session_lost_from_process_lost() {
        let ds = combine_status(ProcessState::Lost, None, None);
        assert_eq!(ds.interpreted, InterpretedStatus::SessionLost);
    }

    #[test]
    fn combine_status_derives_unknown_from_not_started() {
        let ds = combine_status(ProcessState::NotStarted, None, None);
        assert_eq!(ds.interpreted, InterpretedStatus::Unknown);
    }

    #[test]
    fn combine_status_manual_override_present_and_process_state_still_represented() {
        // SPECS §24: manual override takes visual priority but must NOT hide process state.
        let ds = combine_status(
            ProcessState::Running,
            Some(InterpretedStatus::WaitingForInput),
            Some(ManualStatus::Blocked),
        );
        // Both fields must be independently accessible.
        assert_eq!(ds.process, ProcessState::Running);
        assert_eq!(ds.interpreted, InterpretedStatus::WaitingForInput);
        assert_eq!(ds.manual, Some(ManualStatus::Blocked));
    }

    // -------------------------------------------------------------------------
    // running_status (activity-based idle/working) tests
    // -------------------------------------------------------------------------

    #[test]
    fn running_status_recent_output_is_working() {
        // last activity 100ms ago, well within the idle threshold.
        let s = running_status(None, None, Some(1_000), 1_100);
        assert_eq!(s, InterpretedStatus::Working);
    }

    #[test]
    fn running_status_quiet_past_threshold_is_idle() {
        let s = running_status(None, None, Some(1_000), 1_000 + IDLE_AFTER_MS + 1);
        assert_eq!(s, InterpretedStatus::Idle);
    }

    #[test]
    fn running_status_no_output_yet_shows_signal() {
        let s = running_status(Some(InterpretedStatus::Starting), None, None, 5_000);
        assert_eq!(s, InterpretedStatus::Starting);
    }

    #[test]
    fn running_status_sticky_waiting_holds_while_quiet() {
        // Signal arrived at t=1000 (activity also at 1000); now far later but no
        // new activity → still waiting, even though it is "quiet".
        let s = running_status(
            Some(InterpretedStatus::WaitingForInput),
            Some(1_000),
            Some(1_000),
            9_999,
        );
        assert_eq!(s, InterpretedStatus::WaitingForInput);
    }

    #[test]
    fn running_status_signal_superseded_by_later_activity() {
        // Waiting signal at t=1000, but new output at t=2000 → agent resumed.
        let s = running_status(
            Some(InterpretedStatus::WaitingForInput),
            Some(1_000),
            Some(2_000),
            2_050,
        );
        assert_eq!(s, InterpretedStatus::Working);
    }

    #[test]
    fn combine_status_manual_override_with_derived_interpreted() {
        let ds = combine_status(ProcessState::Exited(0), None, Some(ManualStatus::Done));
        assert_eq!(ds.process, ProcessState::Exited(0));
        assert_eq!(ds.interpreted, InterpretedStatus::Completed);
        assert_eq!(ds.manual, Some(ManualStatus::Done));
    }
}
