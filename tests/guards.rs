//! Project-invariant guard tests (SPECS §2, §5).
//!
//! These scan the production source tree at test time to enforce two
//! non-negotiable invariants that are easy to violate by accident:
//!
//! 1. The old placeholder name "Agent Orchestrator" must appear nowhere in
//!    code, UI, config, folders, or branches (SPECS §2).
//! 2. The git layer must never invoke a history-rewriting / commit-creating /
//!    PR-creating git subcommand (SPECS §5). FlightDeck's trustworthiness rests
//!    on never mutating commit history.

use std::fs;
use std::path::{Path, PathBuf};

/// Recursively collect `.rs` files under `dir`.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// SPECS §2: the placeholder name must not appear anywhere in the product
/// surface (we scan `src/` — the spec docs themselves legitimately mention the
/// rule and are excluded).
#[test]
fn no_agent_orchestrator_name_in_source() {
    let src = manifest_dir().join("src");
    let needle = "agent orchestrator";
    for file in rust_files(&src) {
        let contents = fs::read_to_string(&file).unwrap_or_default();
        assert!(
            !contents.to_lowercase().contains(needle),
            "SPECS §2 violation: '{needle}' found in {}",
            file.display()
        );
    }
}

/// SPECS §5: the git layer must not invoke any history-rewriting,
/// commit-creating, or PR-creating git subcommand. We look for these as quoted
/// argument literals (e.g. `"commit"`, `"rebase"`) anywhere under `src/`.
#[test]
fn git_layer_has_no_history_rewriting_subcommands() {
    let src = manifest_dir().join("src");
    // Quoted git-arg literals that must never appear in production source.
    let forbidden = [
        "\"commit\"",
        "\"amend\"",
        "\"--amend\"",
        "\"rebase\"",
        "\"cherry-pick\"",
        "\"cherry\"",
        "\"reset\"",
        "\"filter-branch\"",
        "\"filter-repo\"",
        "\"--force\"",
        "\"-f\"",
        "\"gh\"", // no GitHub PR creation via the gh CLI
    ];
    for file in rust_files(&src) {
        let contents = fs::read_to_string(&file).unwrap_or_default();
        for token in forbidden {
            assert!(
                !contents.contains(token),
                "SPECS §5 violation: forbidden git argument {token} found in {}",
                file.display()
            );
        }
    }
}
