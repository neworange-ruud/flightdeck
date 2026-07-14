//
//  ChatViewModel.swift
//  FlightDeckRemote
//
//  The `@Observable` state backing `AgentChatView` (PRD §5.3 3a). It is the
//  single source of truth for the chat surface: the transcript items, the
//  per-pill expand/collapse state, the Agent·Shell surface selection, and the
//  pagination window.
//
//  Live vs. fixture:
//   - Bound to a `TransportStore` (`bind(to:)`), the items/session metadata
//     are read straight off the store's observable state, so streamed
//     `transcript` / `transcript_append` deltas re-render the list live.
//   - Absent a store (today's default — the transport isn't injected into the
//     view tree yet) it falls back to locally-held items, which the DEBUG
//     `-uitest-fixture-transcript` seam seeds (`loadFixtureIfRequested`).
//
//  All transcript *presentation* logic (timestamp grouping, auto-scroll,
//  pagination gating) lives in `ChatTranscript` (pure, unit-tested); this type
//  only holds state and delegates.
//

import Foundation
import Observation

/// Which per-session surface the chat screen is showing. `shell` is rendered
/// in the switcher but disabled in this task (the real terminal is PRD §5.4,
/// a later task).
enum ChatSurface: Hashable {
    case agent
    case shell
}

@MainActor
@Observable
final class ChatViewModel {

    let projectId: Wire.ProjectId
    let sessionId: Wire.SessionId

    /// Item ids whose activity pill is currently expanded.
    var expandedItemIDs: Set<Wire.ItemId> = []

    /// The selected surface. Only `.agent` is functional this task.
    var surface: ChatSurface = .agent

    // MARK: - Backing state

    /// The bound live store, if any. When set, `items`/metadata read from it.
    private var store: TransportStore?

    /// Locally-held items used when no store is bound (fixtures / previews).
    private(set) var localItems: [Wire.TranscriptItem] = []
    /// The `from_index` of the first loaded item (drives "load earlier").
    private(set) var localFromIndex: UInt64 = 0
    /// Local session metadata for the header when no store is bound.
    private(set) var localSession: LocalSession?

    struct LocalSession {
        var name: String
        var agentType: Wire.AgentType
        var status: Wire.AgentStatus
    }

    init(projectId: Wire.ProjectId, sessionId: Wire.SessionId) {
        self.projectId = projectId
        self.sessionId = sessionId
    }

    // MARK: - Binding

    /// Bind to the live transport store and request the transcript.
    func bind(to store: TransportStore) {
        self.store = store
        // Kick a full transcript load for this session.
        store.requestTranscript(sessionId)
    }

    // MARK: - Derived state (store-first, local fallback)

    /// The transcript items to render. Reading `store.transcripts` here keeps
    /// the view subscribed to live appends via Observation.
    var items: [Wire.TranscriptItem] {
        if let store { return store.transcripts[sessionId] ?? [] }
        return localItems
    }

    /// `from_index` of the first loaded item. The store doesn't expose the
    /// window head yet, so live mode reports 0 (no earlier affordance) until
    /// pagination bookkeeping lands; fixtures use their seeded value.
    var fromIndex: UInt64 {
        store == nil ? localFromIndex : 0
    }

    /// Whether the "load earlier" affordance should show.
    var canLoadEarlier: Bool {
        ChatTranscript.shouldLoadEarlier(fromIndex: fromIndex)
    }

    /// Session display name for the header.
    var sessionName: String {
        liveSession?.name ?? localSession?.name ?? sessionId.rawValue
    }

    /// The agent CLI running this session.
    var agentType: Wire.AgentType {
        liveSession?.agentType ?? localSession?.agentType ?? .claudeCode
    }

    /// The session's current status.
    var status: Wire.AgentStatus {
        liveSession?.status ?? localSession?.status ?? .idle
    }

    /// True when the session is waiting on the human — pins/highlights the
    /// pending permission prompt.
    var isNeedsInput: Bool {
        if case .needsInput = status { return true }
        return pendingPromptItemId != nil
    }

    /// The first pending permission prompt in the transcript, if any. Used to
    /// scroll-to-and-highlight on entry.
    var pendingPromptItemId: Wire.ItemId? {
        items.first(where: { $0.permissionPromptId != nil })?.itemId
    }

    private var liveSession: Wire.SessionState? {
        guard let store, let snapshot = store.snapshot else { return nil }
        for project in snapshot.projects {
            if let session = project.sessions.first(where: { $0.sessionId == sessionId }) {
                return session
            }
        }
        return nil
    }

    // MARK: - Rows

    /// The transcript folded into render rows with sparse timestamp dividers.
    var rows: [ChatRow] { ChatTranscript.rows(for: items) }

    // MARK: - Pill expand/collapse

    func isExpanded(_ itemId: Wire.ItemId) -> Bool {
        expandedItemIDs.contains(itemId)
    }

    func toggleExpanded(_ itemId: Wire.ItemId) {
        if expandedItemIDs.contains(itemId) {
            expandedItemIDs.remove(itemId)
        } else {
            expandedItemIDs.insert(itemId)
        }
    }

    // MARK: - Pagination

    /// Request the page of items immediately before the loaded window. Live
    /// mode asks the desktop; fixture mode prepends its seeded earlier slice.
    func loadEarlier() {
        guard canLoadEarlier else { return }
        if let store {
            store.requestTranscript(sessionId, fromIndex: 0)
            return
        }
        // Fixture: reveal the earlier slice and mark the window head reached.
        let earlier = ChatFixtures.earlierItems()
        localItems = earlier + localItems
        localFromIndex = 0
    }

    // MARK: - Fixtures (DEBUG)

    /// If launched with `-uitest-fixture-transcript` (or running in a preview),
    /// seed a realistic transcript so the screen can be exercised without a
    /// live desktop. Returns whether it seeded — the fixture takes precedence
    /// over any bound store so the UI test is deterministic. No-op in Release.
    @discardableResult
    func loadFixtureIfRequested() -> Bool {
        #if DEBUG
        let env = ProcessInfo.processInfo
        let requested = env.arguments.contains("-uitest-fixture-transcript")
            || env.environment["XCODE_RUNNING_FOR_PREVIEWS"] == "1"
        guard requested else { return false }
        loadFixture()
        return true
        #else
        return false
        #endif
    }

    #if DEBUG
    /// Seed the canned fixture transcript directly (previews / tests).
    func loadFixture() {
        localItems = ChatFixtures.items()
        localFromIndex = ChatFixtures.fromIndex
        localSession = LocalSession(name: "fix-login", agentType: .claudeCode,
                                    status: .needsInput)
    }
    #endif
}
