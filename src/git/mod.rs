//! Git service (T3): the real [`crate::contracts::GitExecutor`] (shelling to
//! `git`) plus the higher-level git workflow logic built on the trait
//! (SPECS §5, §10–§15).
//!
//! SAFETY (SPECS §5): no code path in this module may stage, commit, amend,
//! squash, rebase, cherry-pick, rewrite history, or create GitHub PRs. The
//! `GitExecutor` trait does not even expose such operations. The one commit
//! FlightDeck makes is the empty root commit of a project it has just created
//! in a fresh folder (SPECS §5.3, [`init`]), which has no history to touch.

pub mod branch;
pub mod init;
pub mod remote;
pub mod repo;
pub mod status;
pub mod worktree;
