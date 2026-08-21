//
//  E2E.swift
//  FlightDeckRemote
//
//  Swift mirror of `remote/protocol/src/e2e.rs`: the E2E plane — the
//  application messages exchanged phone <-> desktop as the *plaintext* inside
//  an `EncryptedEnvelope` (`Wire.EncryptedEnvelope`, Relay.swift).
//
//  Two top-level types:
//  * `Wire.DesktopToPhone` — feeds pushed from the Mac to the phone.
//  * `Wire.PhoneCommand` — commands issued by the phone (a `command_id` +
//    `issued_at_ms` + a flattened `CommandBody`).
//
//  Codable strategy (spec §3): internally tagged enums with flattened
//  payloads can't be synthesized, so tagged enums implement Codable by hand.
//  Newtype variants (e.g. `DesktopToPhone.snapshot(StateSnapshot)`) decode
//  the tag from a keyed container and then decode the payload *from the same
//  decoder* (`StateSnapshot(from: decoder)`), which flattens its fields next
//  to `type`; encoding writes the tag and then calls
//  `payload.encode(to: encoder)` — both keyed containers share the same
//  underlying JSON object. Optionals are emitted as explicit `null` (see
//  Common.swift).
//

import Foundation

extension Wire {

    // MARK: - Shared value types

    /// The choice on a permission prompt.
    enum PermissionChoice: String, Codable, Hashable, Sendable {
        case allowOnce = "allow_once"
        case deny
    }

    /// A deep-link target so a notification tap lands on the exact agent/item.
    struct DeepLink: Codable, Hashable, Sendable {
        /// Project to open.
        var projectId: ProjectId
        /// Session to open within the project.
        var sessionId: SessionId
        /// Optional transcript item to scroll to.
        var itemId: ItemId?

        private enum CodingKeys: String, CodingKey {
            case projectId = "project_id"
            case sessionId = "session_id"
            case itemId = "item_id"
        }

        func encode(to encoder: Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(projectId, forKey: .projectId)
            try container.encode(sessionId, forKey: .sessionId)
            try container.encode(itemId, forKey: .itemId) // explicit null
        }
    }

    // MARK: - State snapshot & status

    /// One agent session as shown on a session row.
    struct SessionState: Codable, Hashable, Sendable {
        /// Session id.
        var sessionId: SessionId
        /// Owning project.
        var projectId: ProjectId
        /// Session name (== worktree == branch leaf), e.g. `fix-login`.
        var name: String
        /// Which agent CLI runs here.
        var agentType: AgentType
        /// Current status.
        var status: AgentStatus
        /// Compact git indicators for the row.
        var git: GitIndicators
        /// Wall-clock running time of the current/last turn, in seconds.
        var runningTimeSecs: UInt64
        /// Preview of what a waiting agent is asking (present when needs-input).
        var pendingQuestion: String?

        private enum CodingKeys: String, CodingKey {
            case name, status, git
            case sessionId = "session_id"
            case projectId = "project_id"
            case agentType = "agent_type"
            case runningTimeSecs = "running_time_secs"
            case pendingQuestion = "pending_question"
        }

        func encode(to encoder: Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(sessionId, forKey: .sessionId)
            try container.encode(projectId, forKey: .projectId)
            try container.encode(name, forKey: .name)
            try container.encode(agentType, forKey: .agentType)
            try container.encode(status, forKey: .status)
            try container.encode(git, forKey: .git)
            try container.encode(runningTimeSecs, forKey: .runningTimeSecs)
            try container.encode(pendingQuestion, forKey: .pendingQuestion) // explicit null
        }
    }

    /// Aggregated status for a project (the single dot + plain-language summary).
    struct StatusRollup: Codable, Hashable, Sendable {
        /// Which status dominates the dot (see `RollupDot` precedence).
        var dot: RollupDot
        /// Plain-language summary, e.g. `1 needs input · 1 working · 3 agents`.
        var summary: String
        /// Number of agents working.
        var working: UInt32
        /// Number of agents idle/finished.
        var idle: UInt32
        /// Number of agents needing input.
        var needsInput: UInt32
        /// Number of agents under manual override.
        var manual: UInt32
        /// Total agent count.
        var agentCount: UInt32

        private enum CodingKeys: String, CodingKey {
            case dot, summary, working, idle, manual
            case needsInput = "needs_input"
            case agentCount = "agent_count"
        }
    }

    /// One project and all its sessions.
    struct ProjectState: Codable, Hashable, Sendable {
        /// Project id.
        var projectId: ProjectId
        /// Display name.
        var name: String
        /// Rolled-up status for the project row.
        var rollup: StatusRollup
        /// The project's agent sessions.
        var sessions: [SessionState]

        private enum CodingKeys: String, CodingKey {
            case name, rollup, sessions
            case projectId = "project_id"
        }
    }

    /// A full state snapshot: everything the phone needs to render the
    /// projects and sessions lists. Sent on connect and on `request_snapshot`.
    struct StateSnapshot: Codable, Hashable, Sendable {
        /// Desktop wall-clock time (unix ms) the snapshot was taken.
        var serverTimeMs: Int64
        /// All open projects (each stays live in the background).
        var projects: [ProjectState]

        private enum CodingKeys: String, CodingKey {
            case projects
            case serverTimeMs = "server_time_ms"
        }
    }

    /// An incremental status change for one session.
    struct SessionStatusDelta: Codable, Hashable, Sendable {
        /// Session that changed.
        var sessionId: SessionId
        /// Owning project.
        var projectId: ProjectId
        /// New status.
        var status: AgentStatus
        /// New running time, if it changed.
        var runningTimeSecs: UInt64?
        /// New pending-question preview, if it changed (`null` = unchanged).
        var pendingQuestion: String?

        private enum CodingKeys: String, CodingKey {
            case status
            case sessionId = "session_id"
            case projectId = "project_id"
            case runningTimeSecs = "running_time_secs"
            case pendingQuestion = "pending_question"
        }

        func encode(to encoder: Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(sessionId, forKey: .sessionId)
            try container.encode(projectId, forKey: .projectId)
            try container.encode(status, forKey: .status)
            try container.encode(runningTimeSecs, forKey: .runningTimeSecs) // explicit null
            try container.encode(pendingQuestion, forKey: .pendingQuestion) // explicit null
        }
    }

    /// A batch of incremental status changes.
    struct StatusUpdate: Codable, Hashable, Sendable {
        /// The changed sessions.
        var updates: [SessionStatusDelta]
    }

    /// A project's refreshed roll-up (without resending its sessions).
    struct ProjectRollup: Codable, Hashable, Sendable {
        /// Project id.
        var projectId: ProjectId
        /// Refreshed roll-up.
        var rollup: StatusRollup

        private enum CodingKeys: String, CodingKey {
            case rollup
            case projectId = "project_id"
        }
    }

    /// A batch of project roll-up refreshes.
    struct RollupUpdate: Codable, Hashable, Sendable {
        /// The refreshed projects.
        var projects: [ProjectRollup]
    }

    // MARK: - Transcript feed

    /// Kind of activity a collapsed pill represents (drives the icon).
    enum ActivityKind: String, Codable, Hashable, Sendable {
        case edit
        case command
        case test
        case search
        case other
    }

    /// Binary (allow/deny) vs. multi-option/free-text prompt. Defaults to
    /// `.permission` when the key is absent (Rust `#[serde(default)]`), so
    /// pre-v2 desktop builds' prompts still decode.
    enum PromptKind: String, Codable, Hashable, Sendable {
        /// Binary allow/deny permission request.
        case permission
        /// A multiple-choice question (Claude AskUserQuestion / OpenCode
        /// `question.asked`).
        case question
    }

    /// A selectable option on a permission/question prompt.
    struct PermissionOption: Codable, Hashable, Sendable {
        /// Stable 0-based index within the prompt's option list.
        var index: Int
        /// The binary choice this option maps to (permission prompts only).
        /// `nil` for arbitrary Question options.
        var choice: PermissionChoice?
        /// Human-readable button label, e.g. `Allow once` or an
        /// AskUserQuestion label.
        var label: String
        /// Optional longer description (AskUserQuestion option descriptions).
        var description: String?

        private enum CodingKeys: String, CodingKey {
            case index, choice, label, description
        }

        init(index: Int = 0, choice: PermissionChoice? = nil, label: String,
             description: String? = nil) {
            self.index = index
            self.choice = choice
            self.label = label
            self.description = description
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            // `#[serde(default)]` on the Rust side: absent `index` decodes to 0.
            index = try c.decodeIfPresent(Int.self, forKey: .index) ?? 0
            choice = try c.decodeIfPresent(PermissionChoice.self, forKey: .choice)
            label = try c.decode(String.self, forKey: .label)
            description = try c.decodeIfPresent(String.self, forKey: .description)
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(index, forKey: .index)
            // `skip_serializing_if = "Option::is_none"` on the Rust side — omit
            // (never emit an explicit null) when absent.
            try c.encodeIfPresent(choice, forKey: .choice)
            try c.encode(label, forKey: .label)
            try c.encodeIfPresent(description, forKey: .description)
        }
    }

    /// One question within a (possibly multi-question) prompt form. A Claude
    /// `AskUserQuestion` can carry several of these, each rendered as its own
    /// tab. Mirrors Rust `PromptQuestion`.
    struct PromptQuestion: Codable, Hashable, Sendable {
        /// Short tab header (AskUserQuestion `header`), if present.
        var header: String?
        /// The question text shown to the human.
        var question: String
        /// This question's offered options (>=1), each with a stable 0-based index.
        var options: [PermissionOption]
        /// Whether this question accepts MULTIPLE selected options (a checklist).
        var multiSelect: Bool

        private enum CodingKeys: String, CodingKey {
            case header, question, options
            case multiSelect = "multi_select"
        }

        init(header: String? = nil, question: String, options: [PermissionOption],
             multiSelect: Bool = false) {
            self.header = header
            self.question = question
            self.options = options
            self.multiSelect = multiSelect
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            header = try c.decodeIfPresent(String.self, forKey: .header)
            question = try c.decode(String.self, forKey: .question)
            options = try c.decode([PermissionOption].self, forKey: .options)
            // `#[serde(default)]` on the Rust side: absent means single-select.
            multiSelect = try c.decodeIfPresent(Bool.self, forKey: .multiSelect) ?? false
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            // `skip_serializing_if = "Option::is_none"` — omit when absent.
            try c.encodeIfPresent(header, forKey: .header)
            try c.encode(question, forKey: .question)
            try c.encode(options, forKey: .options)
            try c.encode(multiSelect, forKey: .multiSelect)
        }
    }

    /// The phone's answer to ONE question within a multi-question form. Mirrors
    /// Rust `QuestionAnswer`.
    struct QuestionAnswer: Codable, Hashable, Sendable {
        /// The chosen 0-based option indices for this question, in its own
        /// option order. One entry for a single-select question, several for a
        /// multi-select (checklist) one; empty leaves the question unanswered.
        var optionIndices: [Int]

        private enum CodingKeys: String, CodingKey {
            case optionIndices = "option_indices"
        }

        init(optionIndices: [Int]) {
            self.optionIndices = optionIndices
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            // `#[serde(default)]` on the Rust side: absent decodes to empty.
            optionIndices = try c.decodeIfPresent([Int].self, forKey: .optionIndices) ?? []
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(optionIndices, forKey: .optionIndices)
        }
    }

    /// One item in the cleaned transcript. Internally tagged by `type`.
    enum TranscriptItem: Codable, Hashable, Sendable {
        /// Prose the user sent to the agent.
        case userMessage(itemId: ItemId, text: String, atMs: Int64)
        /// Prose the agent produced (decisions, explanations).
        case agentMessage(itemId: ItemId, text: String, atMs: Int64)
        /// A collapsible activity pill (noisy tool call, summarized).
        case activity(itemId: ItemId, summary: String, detail: String?,
                      body: String?, kind: ActivityKind, atMs: Int64)
        /// An inline permission (binary) or question (N-option/free-text)
        /// prompt awaiting a decision.
        case permissionPrompt(itemId: ItemId, promptId: PromptId, kind: PromptKind,
                              command: String, options: [PermissionOption],
                              allowFreeText: Bool, multiSelect: Bool,
                              questions: [PromptQuestion], atMs: Int64)

        private enum CodingKeys: String, CodingKey {
            case type, text, summary, detail, body, kind, command, options, questions
            case itemId = "item_id"
            case promptId = "prompt_id"
            case allowFreeText = "allow_free_text"
            case multiSelect = "multi_select"
            case atMs = "at_ms"
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let type = try c.decode(String.self, forKey: .type)
            switch type {
            case "user_message":
                self = .userMessage(
                    itemId: try c.decode(ItemId.self, forKey: .itemId),
                    text: try c.decode(String.self, forKey: .text),
                    atMs: try c.decode(Int64.self, forKey: .atMs))
            case "agent_message":
                self = .agentMessage(
                    itemId: try c.decode(ItemId.self, forKey: .itemId),
                    text: try c.decode(String.self, forKey: .text),
                    atMs: try c.decode(Int64.self, forKey: .atMs))
            case "activity":
                self = .activity(
                    itemId: try c.decode(ItemId.self, forKey: .itemId),
                    summary: try c.decode(String.self, forKey: .summary),
                    detail: try c.decodeIfPresent(String.self, forKey: .detail),
                    body: try c.decodeIfPresent(String.self, forKey: .body),
                    kind: try c.decode(ActivityKind.self, forKey: .kind),
                    atMs: try c.decode(Int64.self, forKey: .atMs))
            case "permission_prompt":
                self = .permissionPrompt(
                    itemId: try c.decode(ItemId.self, forKey: .itemId),
                    promptId: try c.decode(PromptId.self, forKey: .promptId),
                    // `#[serde(default)]`: absent `kind` means a pre-v2
                    // desktop's binary permission prompt.
                    kind: try c.decodeIfPresent(PromptKind.self, forKey: .kind) ?? .permission,
                    command: try c.decode(String.self, forKey: .command),
                    options: try c.decode([PermissionOption].self, forKey: .options),
                    allowFreeText: try c.decodeIfPresent(Bool.self, forKey: .allowFreeText) ?? false,
                    // `#[serde(default)]`: absent `multi_select` (a pre-v3
                    // desktop) means a single-select question.
                    multiSelect: try c.decodeIfPresent(Bool.self, forKey: .multiSelect) ?? false,
                    // `#[serde(default)]`: absent `questions` (a pre-v4 desktop)
                    // means the phone falls back to the flat single-question
                    // fields above. The flat fields always mirror `questions[0]`.
                    questions: try c.decodeIfPresent([PromptQuestion].self, forKey: .questions) ?? [],
                    atMs: try c.decode(Int64.self, forKey: .atMs))
            default:
                throw DecodingError.dataCorruptedError(
                    forKey: .type, in: c,
                    debugDescription: "unknown transcript item type: \(type)")
            }
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .userMessage(itemId, text, atMs):
                try c.encode("user_message", forKey: .type)
                try c.encode(itemId, forKey: .itemId)
                try c.encode(text, forKey: .text)
                try c.encode(atMs, forKey: .atMs)
            case let .agentMessage(itemId, text, atMs):
                try c.encode("agent_message", forKey: .type)
                try c.encode(itemId, forKey: .itemId)
                try c.encode(text, forKey: .text)
                try c.encode(atMs, forKey: .atMs)
            case let .activity(itemId, summary, detail, body, kind, atMs):
                try c.encode("activity", forKey: .type)
                try c.encode(itemId, forKey: .itemId)
                try c.encode(summary, forKey: .summary)
                try c.encode(detail, forKey: .detail) // explicit null
                try c.encode(body, forKey: .body) // explicit null
                try c.encode(kind, forKey: .kind)
                try c.encode(atMs, forKey: .atMs)
            case let .permissionPrompt(itemId, promptId, kind, command, options, allowFreeText, multiSelect, questions, atMs):
                try c.encode("permission_prompt", forKey: .type)
                try c.encode(itemId, forKey: .itemId)
                try c.encode(promptId, forKey: .promptId)
                try c.encode(kind, forKey: .kind)
                try c.encode(command, forKey: .command)
                try c.encode(options, forKey: .options)
                try c.encode(allowFreeText, forKey: .allowFreeText)
                try c.encode(multiSelect, forKey: .multiSelect)
                // `skip_serializing_if = "Vec::is_empty"` — omit when empty so
                // the round-trip matches a plain single-question prompt.
                if !questions.isEmpty {
                    try c.encode(questions, forKey: .questions)
                }
                try c.encode(atMs, forKey: .atMs)
            }
        }
    }

    /// A slice of a session's cleaned transcript. Used both for a full load
    /// (`replace == true`) and for incremental appends (`replace == false`).
    struct TranscriptFeed: Codable, Hashable, Sendable {
        /// Session the transcript belongs to.
        var sessionId: SessionId
        /// Ordinal of the first item in `items` within the session's transcript.
        var fromIndex: UInt64
        /// If true, replace any existing items from `from_index` onward.
        var replace: Bool
        /// The transcript items, in order.
        var items: [TranscriptItem]

        private enum CodingKeys: String, CodingKey {
            case replace, items
            case sessionId = "session_id"
            case fromIndex = "from_index"
        }
    }

    // MARK: - Typed events

    /// The typed payload of an agent event. Internally tagged by `type`.
    enum EventKind: Codable, Hashable, Sendable {
        /// The agent stopped and needs the human (most urgent).
        case needsInput(preview: String)
        /// The agent finished its turn.
        case finished(summary: String, filesChanged: UInt32, readyToPush: Bool)
        /// The agent hit an error.
        case error(message: String)

        private enum CodingKeys: String, CodingKey {
            case type, preview, summary, message
            case filesChanged = "files_changed"
            case readyToPush = "ready_to_push"
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let type = try c.decode(String.self, forKey: .type)
            switch type {
            case "needs_input":
                self = .needsInput(preview: try c.decode(String.self, forKey: .preview))
            case "finished":
                self = .finished(
                    summary: try c.decode(String.self, forKey: .summary),
                    filesChanged: try c.decode(UInt32.self, forKey: .filesChanged),
                    readyToPush: try c.decode(Bool.self, forKey: .readyToPush))
            case "error":
                self = .error(message: try c.decode(String.self, forKey: .message))
            default:
                throw DecodingError.dataCorruptedError(
                    forKey: .type, in: c,
                    debugDescription: "unknown event kind: \(type)")
            }
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .needsInput(preview):
                try c.encode("needs_input", forKey: .type)
                try c.encode(preview, forKey: .preview)
            case let .finished(summary, filesChanged, readyToPush):
                try c.encode("finished", forKey: .type)
                try c.encode(summary, forKey: .summary)
                try c.encode(filesChanged, forKey: .filesChanged)
                try c.encode(readyToPush, forKey: .readyToPush)
            case let .error(message):
                try c.encode("error", forKey: .type)
                try c.encode(message, forKey: .message)
            }
        }
    }

    /// A typed status event, mirrored into the Activity feed and driving pushes.
    struct AgentEvent: Codable, Hashable, Sendable {
        /// Stable event id (used for `mark_read` and dedup in the feed).
        var eventId: EventId
        /// The typed payload (nested under `kind`, not flattened).
        var kind: EventKind
        /// Where a tap should land.
        var deepLink: DeepLink
        /// Wall-clock time (unix ms) the event occurred.
        var occurredAtMs: Int64
        /// Short title, e.g. `add-tests finished its turn`.
        var title: String

        private enum CodingKeys: String, CodingKey {
            case kind, title
            case eventId = "event_id"
            case deepLink = "deep_link"
            case occurredAtMs = "occurred_at_ms"
        }
    }

    // MARK: - Shell

    /// Which stream a shell output chunk came from.
    enum ShellStream: String, Codable, Hashable, Sendable {
        case stdout
        case stderr
    }

    /// A chunk of shell output. `data` may contain ANSI escapes; chunks are
    /// ordered per shell by `seq`.
    struct ShellOutput: Codable, Hashable, Sendable {
        /// Owning session.
        var sessionId: SessionId
        /// The shell within the session.
        var shellId: ShellId
        /// Which stream produced this chunk.
        var stream: ShellStream
        /// Monotonic per-shell chunk sequence (starts at 1).
        var seq: UInt64
        /// The output text (may contain ANSI escapes).
        var data: String

        private enum CodingKeys: String, CodingKey {
            case stream, seq, data
            case sessionId = "session_id"
            case shellId = "shell_id"
        }
    }

    /// A shell lifecycle transition. Internally tagged by `type`.
    enum ShellEventKind: Codable, Hashable, Sendable {
        /// The shell opened with the given geometry.
        case opened(cols: UInt16, rows: UInt16)
        /// The shell process exited.
        case exited(code: Int32?)
        /// The shell was closed (by either side).
        case closed

        private enum CodingKeys: String, CodingKey {
            case type, cols, rows, code
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let type = try c.decode(String.self, forKey: .type)
            switch type {
            case "opened":
                self = .opened(
                    cols: try c.decode(UInt16.self, forKey: .cols),
                    rows: try c.decode(UInt16.self, forKey: .rows))
            case "exited":
                self = .exited(code: try c.decodeIfPresent(Int32.self, forKey: .code))
            case "closed":
                self = .closed
            default:
                throw DecodingError.dataCorruptedError(
                    forKey: .type, in: c,
                    debugDescription: "unknown shell event kind: \(type)")
            }
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .opened(cols, rows):
                try c.encode("opened", forKey: .type)
                try c.encode(cols, forKey: .cols)
                try c.encode(rows, forKey: .rows)
            case let .exited(code):
                try c.encode("exited", forKey: .type)
                try c.encode(code, forKey: .code) // explicit null
            case .closed:
                try c.encode("closed", forKey: .type)
            }
        }
    }

    /// A shell lifecycle event for a session's terminal.
    struct ShellEvent: Codable, Hashable, Sendable {
        /// Owning session.
        var sessionId: SessionId
        /// The shell within the session.
        var shellId: ShellId
        /// What happened (nested under `kind`, not flattened).
        var kind: ShellEventKind

        private enum CodingKeys: String, CodingKey {
            case kind
            case sessionId = "session_id"
            case shellId = "shell_id"
        }
    }

    // MARK: - Command acknowledgement

    /// Outcome of a phone command, from the desktop's point of view.
    enum CommandOutcome: String, Codable, Hashable, Sendable {
        /// Received and validated; will be applied.
        case accepted
        /// Applied successfully.
        case applied
        /// Refused for a stated reason (e.g. failed type-to-confirm).
        case rejected
        /// Attempted but failed (e.g. git merge conflict).
        case failed
        /// A command with this id was already processed; ignored idempotently.
        case duplicate
    }

    /// The desktop's acknowledgement of a phone command. Delivery honesty: the
    /// phone shows "not delivered — retry" until it sees an ack for the id.
    struct CommandAck: Codable, Hashable, Sendable {
        /// The command being acknowledged.
        var commandId: CommandId
        /// The outcome.
        var outcome: CommandOutcome
        /// Human-readable detail (reason for reject/fail, or a result note).
        var message: String?

        private enum CodingKeys: String, CodingKey {
            case outcome, message
            case commandId = "command_id"
        }

        func encode(to encoder: Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(commandId, forKey: .commandId)
            try container.encode(outcome, forKey: .outcome)
            try container.encode(message, forKey: .message) // explicit null
        }
    }

    // MARK: - Desktop -> phone feed

    /// A message pushed from the desktop to the phone. Internally tagged by
    /// `type`; each payload's fields are flattened alongside the tag.
    enum DesktopToPhone: Codable, Hashable, Sendable {
        /// Full state snapshot (projects -> sessions).
        case snapshot(StateSnapshot)
        /// Incremental per-session status changes.
        case statusUpdate(StatusUpdate)
        /// Project roll-up refreshes.
        case rollup(RollupUpdate)
        /// A full (or from-cursor) transcript load for a session.
        case transcript(TranscriptFeed)
        /// Incremental transcript items appended to a session.
        case transcriptAppend(TranscriptFeed)
        /// A typed status event (needs-input / finished / error) with deep link.
        case event(AgentEvent)
        /// Full git status detail for a session.
        case gitStatus(GitStatusDetail)
        /// A chunk of shell output.
        case shellOutput(ShellOutput)
        /// A shell lifecycle event.
        case shellEvent(ShellEvent)
        /// Acknowledgement of a phone command.
        case commandAck(CommandAck)

        private enum TagKeys: String, CodingKey {
            case type
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: TagKeys.self)
            let type = try c.decode(String.self, forKey: .type)
            switch type {
            case "snapshot":
                self = .snapshot(try StateSnapshot(from: decoder))
            case "status_update":
                self = .statusUpdate(try StatusUpdate(from: decoder))
            case "rollup":
                self = .rollup(try RollupUpdate(from: decoder))
            case "transcript":
                self = .transcript(try TranscriptFeed(from: decoder))
            case "transcript_append":
                self = .transcriptAppend(try TranscriptFeed(from: decoder))
            case "event":
                self = .event(try AgentEvent(from: decoder))
            case "git_status":
                self = .gitStatus(try GitStatusDetail(from: decoder))
            case "shell_output":
                self = .shellOutput(try ShellOutput(from: decoder))
            case "shell_event":
                self = .shellEvent(try ShellEvent(from: decoder))
            case "command_ack":
                self = .commandAck(try CommandAck(from: decoder))
            default:
                throw DecodingError.dataCorruptedError(
                    forKey: .type, in: c,
                    debugDescription: "unknown desktop_to_phone type: \(type)")
            }
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: TagKeys.self)
            switch self {
            case let .snapshot(payload):
                try c.encode("snapshot", forKey: .type)
                try payload.encode(to: encoder)
            case let .statusUpdate(payload):
                try c.encode("status_update", forKey: .type)
                try payload.encode(to: encoder)
            case let .rollup(payload):
                try c.encode("rollup", forKey: .type)
                try payload.encode(to: encoder)
            case let .transcript(payload):
                try c.encode("transcript", forKey: .type)
                try payload.encode(to: encoder)
            case let .transcriptAppend(payload):
                try c.encode("transcript_append", forKey: .type)
                try payload.encode(to: encoder)
            case let .event(payload):
                try c.encode("event", forKey: .type)
                try payload.encode(to: encoder)
            case let .gitStatus(payload):
                try c.encode("git_status", forKey: .type)
                try payload.encode(to: encoder)
            case let .shellOutput(payload):
                try c.encode("shell_output", forKey: .type)
                try payload.encode(to: encoder)
            case let .shellEvent(payload):
                try c.encode("shell_event", forKey: .type)
                try payload.encode(to: encoder)
            case let .commandAck(payload):
                try c.encode("command_ack", forKey: .type)
                try payload.encode(to: encoder)
            }
        }
    }

    // MARK: - Phone -> desktop commands

    /// The action a phone command performs. Internally tagged by `type`.
    enum CommandBody: Codable, Hashable, Sendable {
        /// Reply / follow-up prose to an agent.
        case reply(sessionId: SessionId, text: String)
        /// Resolve a permission/question prompt. Exactly one of `choice`
        /// (binary fast-path), `optionIndex` (single-select Question, by
        /// position), `optionIndices` (multi-select checklist), or `freeText`
        /// (typed "Other" answer) is set.
        case permissionDecision(sessionId: SessionId, promptId: PromptId,
                                choice: PermissionChoice?, optionIndex: Int?,
                                optionIndices: [Int]?, freeText: String?,
                                answers: [QuestionAnswer]?)
        /// Launch a new agent session.
        case newAgent(projectId: ProjectId, agentType: AgentType, name: String,
                      baseBranch: String, firstTask: String)
        /// Restart the agent in place (fresh process, same worktree/branch).
        case restartAgent(sessionId: SessionId)
        /// Close a session.
        case closeSession(sessionId: SessionId)
        /// Set the cyan manual-override status with a label.
        case setManualStatus(sessionId: SessionId, label: String)
        /// Clear a manual-override status.
        case clearManualStatus(sessionId: SessionId)
        /// Pull the base branch into the worktree (guarded).
        case gitPullBase(sessionId: SessionId)
        /// Merge the session branch back into its base (guarded).
        case gitMergeBack(sessionId: SessionId)
        /// Abandon the worktree. Destructive; requires typed confirmation.
        case gitAbandonWorktree(sessionId: SessionId, confirmName: String)
        /// Open a shell in the session's worktree.
        case shellOpen(sessionId: SessionId, shellId: ShellId, cols: UInt16,
                       rows: UInt16)
        /// Send input (keystrokes/text) to a shell.
        case shellInput(sessionId: SessionId, shellId: ShellId, data: String)
        /// Interrupt the foreground process (Ctrl-C).
        case shellInterrupt(sessionId: SessionId, shellId: ShellId)
        /// Close a shell.
        case shellClose(sessionId: SessionId, shellId: ShellId)
        /// Ask for a fresh snapshot (all projects, or one).
        case requestSnapshot(projectId: ProjectId?)
        /// Ask for a session's transcript (from an item index, or all of it).
        case requestTranscript(sessionId: SessionId, fromIndex: UInt64?)
        /// Mark activity-feed events as read.
        case markRead(eventIds: [EventId])

        private enum CodingKeys: String, CodingKey {
            case type, name, choice, label, cols, rows, data, text
            case sessionId = "session_id"
            case projectId = "project_id"
            case promptId = "prompt_id"
            case agentType = "agent_type"
            case baseBranch = "base_branch"
            case firstTask = "first_task"
            case confirmName = "confirm_name"
            case shellId = "shell_id"
            case fromIndex = "from_index"
            case eventIds = "event_ids"
            case optionIndex = "option_index"
            case optionIndices = "option_indices"
            case freeText = "free_text"
            case answers
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let type = try c.decode(String.self, forKey: .type)
            switch type {
            case "reply":
                self = .reply(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId),
                    text: try c.decode(String.self, forKey: .text))
            case "permission_decision":
                self = .permissionDecision(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId),
                    promptId: try c.decode(PromptId.self, forKey: .promptId),
                    choice: try c.decodeIfPresent(PermissionChoice.self, forKey: .choice),
                    optionIndex: try c.decodeIfPresent(Int.self, forKey: .optionIndex),
                    optionIndices: try c.decodeIfPresent([Int].self, forKey: .optionIndices),
                    freeText: try c.decodeIfPresent(String.self, forKey: .freeText),
                    answers: try c.decodeIfPresent([QuestionAnswer].self, forKey: .answers))
            case "new_agent":
                self = .newAgent(
                    projectId: try c.decode(ProjectId.self, forKey: .projectId),
                    agentType: try c.decode(AgentType.self, forKey: .agentType),
                    name: try c.decode(String.self, forKey: .name),
                    baseBranch: try c.decode(String.self, forKey: .baseBranch),
                    firstTask: try c.decode(String.self, forKey: .firstTask))
            case "restart_agent":
                self = .restartAgent(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId))
            case "close_session":
                self = .closeSession(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId))
            case "set_manual_status":
                self = .setManualStatus(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId),
                    label: try c.decode(String.self, forKey: .label))
            case "clear_manual_status":
                self = .clearManualStatus(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId))
            case "git_pull_base":
                self = .gitPullBase(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId))
            case "git_merge_back":
                self = .gitMergeBack(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId))
            case "git_abandon_worktree":
                self = .gitAbandonWorktree(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId),
                    confirmName: try c.decode(String.self, forKey: .confirmName))
            case "shell_open":
                self = .shellOpen(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId),
                    shellId: try c.decode(ShellId.self, forKey: .shellId),
                    cols: try c.decode(UInt16.self, forKey: .cols),
                    rows: try c.decode(UInt16.self, forKey: .rows))
            case "shell_input":
                self = .shellInput(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId),
                    shellId: try c.decode(ShellId.self, forKey: .shellId),
                    data: try c.decode(String.self, forKey: .data))
            case "shell_interrupt":
                self = .shellInterrupt(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId),
                    shellId: try c.decode(ShellId.self, forKey: .shellId))
            case "shell_close":
                self = .shellClose(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId),
                    shellId: try c.decode(ShellId.self, forKey: .shellId))
            case "request_snapshot":
                self = .requestSnapshot(
                    projectId: try c.decodeIfPresent(ProjectId.self, forKey: .projectId))
            case "request_transcript":
                self = .requestTranscript(
                    sessionId: try c.decode(SessionId.self, forKey: .sessionId),
                    fromIndex: try c.decodeIfPresent(UInt64.self, forKey: .fromIndex))
            case "mark_read":
                self = .markRead(
                    eventIds: try c.decode([EventId].self, forKey: .eventIds))
            default:
                throw DecodingError.dataCorruptedError(
                    forKey: .type, in: c,
                    debugDescription: "unknown command type: \(type)")
            }
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .reply(sessionId, text):
                try c.encode("reply", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
                try c.encode(text, forKey: .text)
            case let .permissionDecision(sessionId, promptId, choice, optionIndex, optionIndices, freeText, answers):
                try c.encode("permission_decision", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
                try c.encode(promptId, forKey: .promptId)
                // `skip_serializing_if = "Option::is_none"` on the Rust side —
                // omit whichever field is unset (never explicit null).
                try c.encodeIfPresent(choice, forKey: .choice)
                try c.encodeIfPresent(optionIndex, forKey: .optionIndex)
                try c.encodeIfPresent(optionIndices, forKey: .optionIndices)
                try c.encodeIfPresent(freeText, forKey: .freeText)
                try c.encodeIfPresent(answers, forKey: .answers)
            case let .newAgent(projectId, agentType, name, baseBranch, firstTask):
                try c.encode("new_agent", forKey: .type)
                try c.encode(projectId, forKey: .projectId)
                try c.encode(agentType, forKey: .agentType)
                try c.encode(name, forKey: .name)
                try c.encode(baseBranch, forKey: .baseBranch)
                try c.encode(firstTask, forKey: .firstTask)
            case let .restartAgent(sessionId):
                try c.encode("restart_agent", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
            case let .closeSession(sessionId):
                try c.encode("close_session", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
            case let .setManualStatus(sessionId, label):
                try c.encode("set_manual_status", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
                try c.encode(label, forKey: .label)
            case let .clearManualStatus(sessionId):
                try c.encode("clear_manual_status", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
            case let .gitPullBase(sessionId):
                try c.encode("git_pull_base", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
            case let .gitMergeBack(sessionId):
                try c.encode("git_merge_back", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
            case let .gitAbandonWorktree(sessionId, confirmName):
                try c.encode("git_abandon_worktree", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
                try c.encode(confirmName, forKey: .confirmName)
            case let .shellOpen(sessionId, shellId, cols, rows):
                try c.encode("shell_open", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
                try c.encode(shellId, forKey: .shellId)
                try c.encode(cols, forKey: .cols)
                try c.encode(rows, forKey: .rows)
            case let .shellInput(sessionId, shellId, data):
                try c.encode("shell_input", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
                try c.encode(shellId, forKey: .shellId)
                try c.encode(data, forKey: .data)
            case let .shellInterrupt(sessionId, shellId):
                try c.encode("shell_interrupt", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
                try c.encode(shellId, forKey: .shellId)
            case let .shellClose(sessionId, shellId):
                try c.encode("shell_close", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
                try c.encode(shellId, forKey: .shellId)
            case let .requestSnapshot(projectId):
                try c.encode("request_snapshot", forKey: .type)
                try c.encode(projectId, forKey: .projectId) // explicit null
            case let .requestTranscript(sessionId, fromIndex):
                try c.encode("request_transcript", forKey: .type)
                try c.encode(sessionId, forKey: .sessionId)
                try c.encode(fromIndex, forKey: .fromIndex) // explicit null
            case let .markRead(eventIds):
                try c.encode("mark_read", forKey: .type)
                try c.encode(eventIds, forKey: .eventIds)
            }
        }
    }

    /// A command issued by the phone. Every command carries a client-generated
    /// `CommandId` (the idempotency key). On the wire the `CommandBody` is
    /// flattened next to `command_id`/`issued_at_ms`:
    /// `{"command_id":"…","issued_at_ms":…,"type":"reply","session_id":"…","text":"…"}`.
    struct PhoneCommand: Codable, Hashable, Sendable {
        /// Client-generated, unique per logical command; the idempotency key.
        var commandId: CommandId
        /// Phone wall-clock time (unix ms) at issue, for latency/ordering.
        var issuedAtMs: Int64
        /// What the command does (flattened on the wire).
        var body: CommandBody

        private enum CodingKeys: String, CodingKey {
            case commandId = "command_id"
            case issuedAtMs = "issued_at_ms"
        }

        init(commandId: CommandId, issuedAtMs: Int64, body: CommandBody) {
            self.commandId = commandId
            self.issuedAtMs = issuedAtMs
            self.body = body
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            commandId = try c.decode(CommandId.self, forKey: .commandId)
            issuedAtMs = try c.decode(Int64.self, forKey: .issuedAtMs)
            body = try CommandBody(from: decoder) // flattened
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(commandId, forKey: .commandId)
            try c.encode(issuedAtMs, forKey: .issuedAtMs)
            try body.encode(to: encoder) // flattened
        }
    }
}
