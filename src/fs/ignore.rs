//! Append-only `.gitignore` updater (SPECS §6).
//!
//! Adds the two required entries only if missing, preserving all existing
//! contents and order, and reports whether anything changed.

use crate::contracts::{FileSystem, Result};
use std::path::Path;

/// Required entry: the ignored runtime state file.
pub const STATE_IGNORE_ENTRY: &str = ".flightdeck/state.json";
/// Required entry: the ignored managed worktrees directory.
pub const WORKTREES_IGNORE_ENTRY: &str = ".flightdeck/worktrees/";
/// Per-worktree agent status file written by lifecycle hooks/plugins.
pub const STATUS_IGNORE_ENTRY: &str = ".flightdeck/agent-status";
/// Generated launch-scoped hook/plugin artifacts.
pub const STATUS_RUNTIME_IGNORE_ENTRY: &str = ".flightdeck/runtime/";

/// Ensure a single entry is present in `<repo_root>/.gitignore`, appending it
/// only if missing (same append-only contract as
/// [`ensure_flightdeck_gitignore`]). Returns whether the file changed.
pub fn ensure_gitignore_entry(fs: &dyn FileSystem, repo_root: &Path, entry: &str) -> Result<bool> {
    let gitignore_path = repo_root.join(".gitignore");
    let existing = if fs.exists(&gitignore_path) {
        fs.read_to_string(&gitignore_path)?
    } else {
        String::new()
    };
    if existing.lines().map(str::trim).any(|l| l == entry) {
        return Ok(false);
    }
    // `append_line` inserts a separating newline itself when the existing
    // content lacks a trailing one, honouring the append-only contract.
    fs.append_line(&gitignore_path, entry)?;
    Ok(true)
}

/// Result of an attempted `.gitignore` update.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitignoreUpdate {
    /// Whether the file was modified.
    pub changed: bool,
    /// The entries that were appended.
    pub added: Vec<String>,
}

/// Ensure the required FlightDeck entries are present in `<repo_root>/.gitignore`,
/// appending only the missing ones (SPECS §6).
pub fn ensure_flightdeck_gitignore(
    fs: &dyn FileSystem,
    repo_root: &Path,
) -> Result<GitignoreUpdate> {
    let gitignore_path = repo_root.join(".gitignore");

    // Read existing content if the file exists; treat missing file as empty.
    let existing = if fs.exists(&gitignore_path) {
        fs.read_to_string(&gitignore_path)?
    } else {
        String::new()
    };

    // Collect trimmed lines for exact-match comparison.
    let trimmed_lines: Vec<&str> = existing.lines().map(str::trim).collect();

    let required = [
        STATE_IGNORE_ENTRY,
        WORKTREES_IGNORE_ENTRY,
        STATUS_IGNORE_ENTRY,
        STATUS_RUNTIME_IGNORE_ENTRY,
    ];
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|entry| !trimmed_lines.contains(entry))
        .collect();

    if missing.is_empty() {
        return Ok(GitignoreUpdate {
            changed: false,
            added: Vec::new(),
        });
    }

    // Append missing entries — one per call to `append_line` to honour the
    // trait's append-only, never-rewrite contract.
    for entry in &missing {
        fs.append_line(&gitignore_path, entry)?;
    }

    Ok(GitignoreUpdate {
        changed: true,
        added: missing.iter().map(|s| s.to_string()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeFs;
    use std::path::Path;

    const REPO_ROOT: &str = "/repo";
    const GITIGNORE: &str = "/repo/.gitignore";

    // Helper: run ensure and return the update result plus the file contents.
    fn run(fs: &FakeFs) -> (GitignoreUpdate, String) {
        let update =
            ensure_flightdeck_gitignore(fs, Path::new(REPO_ROOT)).expect("should not fail");
        let contents = fs.file_contents(Path::new(GITIGNORE)).unwrap_or_default();
        (update, contents)
    }

    // §26: creates the file when absent
    #[test]
    fn creates_file_when_absent() {
        let fs = FakeFs::new();
        let (update, contents) = run(&fs);

        assert!(update.changed);
        assert_eq!(
            update.added,
            vec![
                STATE_IGNORE_ENTRY,
                WORKTREES_IGNORE_ENTRY,
                STATUS_IGNORE_ENTRY,
                STATUS_RUNTIME_IGNORE_ENTRY,
            ]
        );
        assert!(contents.contains(STATE_IGNORE_ENTRY));
        assert!(contents.contains(WORKTREES_IGNORE_ENTRY));
    }

    // §26: appends missing entries when .gitignore lacks them
    #[test]
    fn appends_both_when_file_exists_but_empty() {
        let fs = FakeFs::new().with_file(GITIGNORE, "");
        let (update, contents) = run(&fs);

        assert!(update.changed);
        assert_eq!(
            update.added,
            vec![
                STATE_IGNORE_ENTRY,
                WORKTREES_IGNORE_ENTRY,
                STATUS_IGNORE_ENTRY,
                STATUS_RUNTIME_IGNORE_ENTRY,
            ]
        );
        assert!(contents.contains(STATE_IGNORE_ENTRY));
        assert!(contents.contains(WORKTREES_IGNORE_ENTRY));
    }

    // §26: does NOT duplicate an entry that already exists
    #[test]
    fn does_not_duplicate_existing_entry() {
        // One entry already present; only the other should be appended.
        let initial = format!("{STATE_IGNORE_ENTRY}\n");
        let fs = FakeFs::new().with_file(GITIGNORE, initial.as_str());
        let (update, contents) = run(&fs);

        assert!(update.changed);
        assert_eq!(
            update.added,
            vec![
                WORKTREES_IGNORE_ENTRY,
                STATUS_IGNORE_ENTRY,
                STATUS_RUNTIME_IGNORE_ENTRY,
            ]
        );

        // STATE_IGNORE_ENTRY must appear exactly once.
        let count = contents
            .lines()
            .filter(|l| l.trim() == STATE_IGNORE_ENTRY)
            .count();
        assert_eq!(count, 1, "STATE_IGNORE_ENTRY duplicated");
    }

    // §26: does NOT duplicate when both entries are already present
    #[test]
    fn no_change_when_both_already_present() {
        let initial = format!(
            "{STATE_IGNORE_ENTRY}\n{WORKTREES_IGNORE_ENTRY}\n{STATUS_IGNORE_ENTRY}\n{STATUS_RUNTIME_IGNORE_ENTRY}\n"
        );
        let fs = FakeFs::new().with_file(GITIGNORE, initial.as_str());
        let (update, _) = run(&fs);

        assert!(!update.changed);
        assert!(update.added.is_empty());
    }

    // §26: preserves unrelated existing lines and their order
    #[test]
    fn preserves_unrelated_lines_and_order() {
        let initial = "/target\nnode_modules\n";
        let fs = FakeFs::new().with_file(GITIGNORE, initial);
        let (update, contents) = run(&fs);

        assert!(update.changed);

        let lines: Vec<&str> = contents.lines().collect();

        // Existing lines must still be at the beginning, in original order.
        assert_eq!(lines[0], "/target");
        assert_eq!(lines[1], "node_modules");

        // New entries appended after.
        assert!(lines.contains(&STATE_IGNORE_ENTRY));
        assert!(lines.contains(&WORKTREES_IGNORE_ENTRY));

        // The index of the new entries must be after the existing ones.
        let target_idx = lines.iter().position(|l| *l == "/target").unwrap();
        let node_idx = lines.iter().position(|l| *l == "node_modules").unwrap();
        let state_idx = lines.iter().position(|l| *l == STATE_IGNORE_ENTRY).unwrap();
        let wt_idx = lines
            .iter()
            .position(|l| *l == WORKTREES_IGNORE_ENTRY)
            .unwrap();

        assert!(target_idx < state_idx);
        assert!(node_idx < state_idx);
        assert!(target_idx < wt_idx);
        assert!(node_idx < wt_idx);
    }

    // Regression: a .gitignore whose last line has no trailing newline must not
    // have the first appended entry glued onto it.
    #[test]
    fn appends_cleanly_when_file_lacks_trailing_newline() {
        // Note: no trailing '\n' after "node_modules".
        let fs = FakeFs::new().with_file(GITIGNORE, "/target\nnode_modules");
        let (update, contents) = run(&fs);

        assert!(update.changed);
        let lines: Vec<&str> = contents.lines().collect();
        // The last pre-existing line must survive intact on its own line.
        assert!(
            lines.contains(&"node_modules"),
            "existing last line was corrupted: {contents:?}"
        );
        // And the appended entries are each on their own line.
        assert!(lines.contains(&STATE_IGNORE_ENTRY));
        assert!(lines.contains(&WORKTREES_IGNORE_ENTRY));
        // No line accidentally concatenated the two.
        assert!(
            !lines
                .iter()
                .any(|l| l.contains("node_modules") && l.len() > "node_modules".len()),
            "an entry was glued onto node_modules: {contents:?}"
        );
    }

    // §26: changed=true with correct added list when it appends
    #[test]
    fn returns_correct_added_list() {
        let fs = FakeFs::new();
        let update =
            ensure_flightdeck_gitignore(&fs, Path::new(REPO_ROOT)).expect("should not fail");

        assert!(update.changed);
        assert_eq!(update.added.len(), 4);
        assert!(update.added.contains(&STATE_IGNORE_ENTRY.to_string()));
        assert!(update.added.contains(&WORKTREES_IGNORE_ENTRY.to_string()));
        assert!(update.added.contains(&STATUS_IGNORE_ENTRY.to_string()));
        assert!(update
            .added
            .contains(&STATUS_RUNTIME_IGNORE_ENTRY.to_string()));
    }

    // §26: changed=false, added=[] when nothing to do
    #[test]
    fn returns_no_change_when_nothing_to_do() {
        let initial = format!(
            "{STATE_IGNORE_ENTRY}\n{WORKTREES_IGNORE_ENTRY}\n{STATUS_IGNORE_ENTRY}\n{STATUS_RUNTIME_IGNORE_ENTRY}\n"
        );
        let fs = FakeFs::new().with_file(GITIGNORE, initial.as_str());
        let update =
            ensure_flightdeck_gitignore(&fs, Path::new(REPO_ROOT)).expect("should not fail");

        assert!(!update.changed);
        assert!(update.added.is_empty());
    }

    // Extra: entries with surrounding whitespace must still be detected as present.
    #[test]
    fn trimmed_match_prevents_duplicate() {
        // Entry present with leading/trailing spaces — should still be detected.
        let initial = format!(
            "  {STATE_IGNORE_ENTRY}  \n{WORKTREES_IGNORE_ENTRY}\n{STATUS_IGNORE_ENTRY}\n{STATUS_RUNTIME_IGNORE_ENTRY}\n"
        );
        let fs = FakeFs::new().with_file(GITIGNORE, initial.as_str());
        let (update, _) = run(&fs);

        assert!(
            !update.changed,
            "should detect both entries despite whitespace"
        );
    }
}
