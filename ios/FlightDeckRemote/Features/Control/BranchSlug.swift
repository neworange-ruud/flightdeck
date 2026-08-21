//
//  BranchSlug.swift
//  FlightDeckRemote
//
//  Swift port of the desktop's slug + branch-name rules
//  (`src/git/branch.rs`, SPECS §11/§26), so the New-Agent screen's live
//  "flightdeck/<slug>" preview matches exactly what the desktop will create
//  when the `new_agent` command lands (PRD §5.5).
//
//  Rules (byte-for-byte the desktop's `slugify`):
//   - ASCII alphanumerics are lowercased and kept;
//   - non-ASCII alphanumerics are lowercased (possibly multi-character) and
//     kept;
//   - every run of non-alphanumeric characters collapses to a single hyphen;
//   - leading and trailing hyphens are trimmed.
//

import Foundation

enum BranchSlug {

    /// The worktree/branch prefix the desktop applies (SPECS §11), shown in
    /// the New-Agent screen's live branch preview.
    static let defaultPrefix = "flightdeck/"

    /// Generate a task slug from a free-form session name (lowercase,
    /// hyphenated, alphanumeric-only). Mirrors `src/git/branch.rs::slugify`.
    static func slugify(_ name: String) -> String {
        var out = ""
        var prevHyphen = false
        for ch in name {
            if ch.isASCII, ch.isLetter || ch.isNumber {
                out.append(contentsOf: ch.lowercased())
                prevHyphen = false
            } else if ch.isLetter || ch.isNumber {
                // Non-ASCII alphanumerics: lowercase but keep.
                out.append(contentsOf: ch.lowercased())
                prevHyphen = false
            } else {
                // Any non-alphanumeric run → a single hyphen.
                if !prevHyphen {
                    out.append("-")
                    prevHyphen = true
                }
            }
        }
        // Trim leading/trailing hyphens (mirrors `trim_matches('-')`).
        while out.hasPrefix("-") { out.removeFirst() }
        while out.hasSuffix("-") { out.removeLast() }
        return out
    }

    /// Build the full branch name `<prefix><slug>` (mirrors
    /// `src/git/branch.rs::branch_name`).
    static func branchName(prefix: String, slug: String) -> String {
        "\(prefix)\(slug)"
    }
}
