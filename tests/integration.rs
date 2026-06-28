//! Integration test suite (T10) — exercised against real temporary git repos
//! (SPECS §26 "Integration Tests"). Each submodule covers one area.
//!
//! `tests/integration.rs` is a test-crate root, so submodules are pointed at the
//! `tests/integration/` directory explicitly via `#[path]`.

#[path = "integration/util.rs"]
mod util;

#[path = "integration/init.rs"]
mod init;
#[path = "integration/merge_preconditions.rs"]
mod merge_preconditions;
#[path = "integration/push.rs"]
mod push;
#[path = "integration/recovery.rs"]
mod recovery;
#[path = "integration/worktree.rs"]
mod worktree;
