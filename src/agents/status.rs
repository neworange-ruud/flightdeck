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

    #[test]
    fn combine_status_manual_override_with_derived_interpreted() {
        let ds = combine_status(ProcessState::Exited(0), None, Some(ManualStatus::Done));
        assert_eq!(ds.process, ProcessState::Exited(0));
        assert_eq!(ds.interpreted, InterpretedStatus::Completed);
        assert_eq!(ds.manual, Some(ManualStatus::Done));
    }
}
