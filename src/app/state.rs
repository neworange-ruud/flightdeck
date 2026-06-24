//! Headless application state (T7, SPECS §3, §24, §25).
//!
//! Designed and implemented in Phase 2. Holds the Agent Tabs (the §3 invariant:
//! 1 tab = 1 worktree = 1 branch = 1 primary agent), the selected tab and child
//! terminal, the current input mode, and persistent warnings (e.g. dirty-base →
//! merge disabled, SPECS §13).
