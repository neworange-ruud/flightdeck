//! Display-status combination (SPECS §24).

use crate::contracts::{InterpretedStatus, ManualStatus, ProcessState};

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
