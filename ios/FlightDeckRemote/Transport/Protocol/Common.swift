//
//  Common.swift
//  FlightDeckRemote
//
//  Swift mirror of `remote/protocol/src/common.rs`: version constants, roles,
//  agent identity/status, and git status detail.
//
//  Serde conventions (spec §3):
//  * enums are internally tagged by `type` — except `AgentStatus`, tagged by
//    `state`;
//  * all wire names are snake_case;
//  * optionals are ALWAYS emitted, as explicit `null` (the golden fixtures
//    require `"field": null` to be present) — so any type with an Optional
//    stored property implements `encode(to:)` by hand and encodes the
//    Optional through the generic `encode(_:forKey:)`, which writes `null`
//    for `.none` (unlike synthesized Codable, which uses `encodeIfPresent`
//    and would drop the key).
//

import Foundation

extension Wire {

    // MARK: - Version constants

    /// Protocol version this build speaks and prefers.
    static let protocolVersion: UInt16 = 2
    /// Oldest protocol version this build can still interoperate with.
    static let minSupportedVersion: UInt16 = 1
    /// Newest protocol version this build can interoperate with.
    static let maxSupportedVersion: UInt16 = 2

    // MARK: - Role / agent identity

    /// The two roles that connect to the relay for a given pairing.
    enum Role: String, Codable, Hashable, Sendable {
        case desktop
        case phone
    }

    /// Which agent CLI backs a session.
    enum AgentType: String, Codable, Hashable, Sendable {
        case claudeCode = "claude_code"
        case opencode
        case codex
    }

    // MARK: - Agent status

    /// FlightDeck's four agent states. Internally tagged by `state` (the sole
    /// exception to the `type` tag, spec §3). `manual` carries the user-set
    /// label alongside the tag: `{"state":"manual","label":"…"}`.
    enum AgentStatus: Codable, Hashable, Sendable {
        /// Red spinner: the agent is actively running a turn.
        case working
        /// Green: the turn is done; waiting for a prompt.
        case idle
        /// Orange glow: stopped, asking the human (permission / question).
        case needsInput
        /// Cyan: user-flagged manual override with a short label.
        case manual(label: String)

        private enum CodingKeys: String, CodingKey {
            case state
            case label
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            let state = try container.decode(String.self, forKey: .state)
            switch state {
            case "working":
                self = .working
            case "idle":
                self = .idle
            case "needs_input":
                self = .needsInput
            case "manual":
                self = .manual(label: try container.decode(String.self, forKey: .label))
            default:
                throw DecodingError.dataCorruptedError(
                    forKey: .state, in: container,
                    debugDescription: "unknown agent status state: \(state)")
            }
        }

        func encode(to encoder: Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case .working:
                try container.encode("working", forKey: .state)
            case .idle:
                try container.encode("idle", forKey: .state)
            case .needsInput:
                try container.encode("needs_input", forKey: .state)
            case .manual(let label):
                try container.encode("manual", forKey: .state)
                try container.encode(label, forKey: .label)
            }
        }
    }

    /// Which status dominates a project's roll-up dot. Precedence, high to
    /// low: needs-input > working > manual > idle.
    enum RollupDot: String, Codable, Hashable, Sendable {
        case needsInput = "needs_input"
        case working
        case manual
        case idle
    }

    // MARK: - Git

    /// Compact git indicators shown on a session row.
    struct GitIndicators: Codable, Hashable, Sendable {
        /// Branch name, if the worktree has one checked out.
        var branch: String?
        /// Count of added (new) files (`+`).
        var added: UInt32
        /// Count of modified files (`~`).
        var modified: UInt32
        /// Count of removed files (`-`).
        var removed: UInt32
        /// Commits ahead of upstream.
        var ahead: UInt32
        /// Commits behind upstream.
        var behind: UInt32
        /// Commits of drift from the base branch.
        var drift: UInt32
        /// Whether the branch has an upstream (`false` renders `no-upstream`).
        var hasUpstream: Bool

        /// True when there are no uncommitted file changes (renders `clean`).
        var isClean: Bool { added == 0 && modified == 0 && removed == 0 }

        private enum CodingKeys: String, CodingKey {
            case branch, added, modified, removed, ahead, behind, drift
            case hasUpstream = "has_upstream"
        }

        init(branch: String?, added: UInt32, modified: UInt32, removed: UInt32,
             ahead: UInt32, behind: UInt32, drift: UInt32, hasUpstream: Bool) {
            self.branch = branch
            self.added = added
            self.modified = modified
            self.removed = removed
            self.ahead = ahead
            self.behind = behind
            self.drift = drift
            self.hasUpstream = hasUpstream
        }

        func encode(to encoder: Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(branch, forKey: .branch) // explicit null
            try container.encode(added, forKey: .added)
            try container.encode(modified, forKey: .modified)
            try container.encode(removed, forKey: .removed)
            try container.encode(ahead, forKey: .ahead)
            try container.encode(behind, forKey: .behind)
            try container.encode(drift, forKey: .drift)
            try container.encode(hasUpstream, forKey: .hasUpstream)
        }
    }

    /// Per-file change kind in a full git status.
    enum GitFileStatus: String, Codable, Hashable, Sendable {
        case added
        case modified
        case deleted
        case renamed
        case untracked
    }

    /// One changed file in a git status detail.
    struct GitFileChange: Codable, Hashable, Sendable {
        /// Repo-relative path (new path for renames).
        var path: String
        /// The change kind.
        var status: GitFileStatus
        /// Lines added in this file.
        var addedLines: UInt32
        /// Lines removed in this file.
        var removedLines: UInt32

        private enum CodingKeys: String, CodingKey {
            case path, status
            case addedLines = "added_lines"
            case removedLines = "removed_lines"
        }
    }

    /// Full, read-only git status for a session's worktree.
    struct GitStatusDetail: Codable, Hashable, Sendable {
        /// The session this status describes.
        var sessionId: SessionId
        /// Current branch.
        var branch: String?
        /// Base branch the worktree was created from.
        var baseBranch: String?
        /// Whether the branch has an upstream.
        var hasUpstream: Bool
        /// Commits ahead of upstream.
        var ahead: UInt32
        /// Commits behind upstream.
        var behind: UInt32
        /// Commits of drift from base.
        var drift: UInt32
        /// The changed files.
        var files: [GitFileChange]

        private enum CodingKeys: String, CodingKey {
            case branch, ahead, behind, drift, files
            case sessionId = "session_id"
            case baseBranch = "base_branch"
            case hasUpstream = "has_upstream"
        }

        func encode(to encoder: Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(sessionId, forKey: .sessionId)
            try container.encode(branch, forKey: .branch) // explicit null
            try container.encode(baseBranch, forKey: .baseBranch) // explicit null
            try container.encode(hasUpstream, forKey: .hasUpstream)
            try container.encode(ahead, forKey: .ahead)
            try container.encode(behind, forKey: .behind)
            try container.encode(drift, forKey: .drift)
            try container.encode(files, forKey: .files)
        }
    }
}
