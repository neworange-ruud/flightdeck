//! Output→status classification and status combination (SPECS §24).

use crate::contracts::{InterpretedStatus, ManualStatus, ProcessState, StatusPatterns};

/// Classify agent output against the config `status_patterns` via substring
/// match (SPECS §24). Returns the matched interpreted status, if any. Error
/// patterns take precedence, then waiting, then completed (caller-defined
/// ordering is fixed here).
pub fn classify_output(patterns: &StatusPatterns, output: &str) -> Option<InterpretedStatus> {
    let _ = (patterns, output);
    todo!("T4: substring match -> Failed/WaitingForInput/Completed")
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
pub fn combine_status(
    process: ProcessState,
    interpreted: Option<InterpretedStatus>,
    manual: Option<ManualStatus>,
) -> DisplayStatus {
    let _ = (process, interpreted, manual);
    todo!("T4: derive interpreted from process when none, keep manual separate")
}
