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

    // MARK: - Compose + send state (PRD §5.3 / §5.8)

    /// The compose field's current text (bound by the compose bar).
    var draft: String = ""

    /// Optimistically-appended outgoing user messages, in send order. Merged
    /// into the rendered transcript until the desktop echoes them back.
    private(set) var outgoing: [OutgoingMessage] = []

    /// Per-prompt inline permission-decision state (spinner / resolved / stale).
    private(set) var permissionActions: [Wire.PromptId: PermissionActionState] = [:]

    // MARK: - Send dependencies

    /// The command sender (live `TransportStore`, or a DEBUG scripted sender in
    /// fixture / UI-test mode). `nil` until configured.
    private var sender: (any ChatCommandSending)?
    /// Visible commands-paused gate (PRD §8): sends are blocked + the compose
    /// bar shows the paused state when the link isn't live.
    private var pausedGate: CommandsPausedGate?

    /// Wall-clock source for optimistic `issued_at` stamps (injectable for tests).
    private let now: () -> Int64

    // Command-id ↔ target routing for delivery reconciliation.
    private var commandToOutgoing: [Wire.CommandId: Wire.ItemId] = [:]
    private var commandToPrompt: [Wire.CommandId: Wire.PromptId] = [:]
    private var permissionChoice: [Wire.PromptId: Wire.PermissionChoice] = [:]
    private var permissionCommandId: [Wire.PromptId: Wire.CommandId] = [:]

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

    init(projectId: Wire.ProjectId, sessionId: Wire.SessionId,
         now: @escaping () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }) {
        self.projectId = projectId
        self.sessionId = sessionId
        self.now = now
    }

    // MARK: - Binding

    /// Bind to the live transport store and request the transcript. The store
    /// is also the command sender and the source for the commands-paused gate.
    func bind(to store: TransportStore) {
        self.store = store
        self.sender = store
        self.pausedGate = CommandsPausedGate(source: store)
        // Kick a full transcript load for this session.
        store.requestTranscript(sessionId)
    }

    /// Configure the send path explicitly (the DEBUG fixture / UI-test seam,
    /// where there is no live store). Additive — leaves fixture items intact.
    func configureSend(sender: any ChatCommandSending, pausedGate: CommandsPausedGate) {
        self.sender = sender
        self.pausedGate = pausedGate
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

    /// The session's *current* pending permission prompt id — the last prompt in
    /// the transcript (the desktop holds one at a time). Only this prompt's
    /// Allow/Deny buttons are live; older prompts are historical.
    var currentPendingPromptId: Wire.PromptId? {
        items.last(where: { $0.permissionPromptId != nil })?.permissionPromptId
    }

    /// Whether commands are currently paused (link down, PRD §8). Defaults to
    /// paused (safe) until a gate is configured.
    var commandsPaused: Bool {
        pausedGate?.commandsPaused ?? true
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

    /// The authoritative transcript plus any optimistic outgoing messages not
    /// yet echoed back by the desktop (`ChatSendLogic.visibleOutgoing`).
    var displayItems: [Wire.TranscriptItem] {
        let authoritative = items
        let visible = ChatSendLogic.visibleOutgoing(outgoing, against: authoritative)
        let optimistic = visible.map { msg in
            Wire.TranscriptItem.userMessage(itemId: msg.localId, text: msg.text,
                                            atMs: msg.issuedAtMs)
        }
        return authoritative + optimistic
    }

    /// The transcript (incl. optimistic sends) folded into render rows.
    var rows: [ChatRow] { ChatTranscript.rows(for: displayItems) }

    /// The send state of an optimistic outgoing row, if `itemId` is one.
    func outgoingState(forItemId itemId: Wire.ItemId) -> OutgoingState? {
        outgoing.first(where: { $0.localId == itemId })?.state
    }

    /// The inline permission-action state for a prompt (idle if untouched).
    func permissionActionState(_ promptId: Wire.PromptId) -> PermissionActionState {
        permissionActions[promptId] ?? .idle
    }

    /// Whether a prompt's Allow/Deny buttons should be live: it is the session's
    /// current pending prompt, the link is up, and no decision is in flight.
    func isPermissionActionable(_ promptId: Wire.PromptId) -> Bool {
        guard promptId == currentPendingPromptId, !commandsPaused else { return false }
        switch permissionActionState(promptId) {
        case .idle, .failed: return true
        case .sending, .resolved, .stale: return false
        }
    }

    // MARK: - Sending (reply / follow-up)

    /// Send the current `draft` as a reply / follow-up: optimistically append it
    /// (marked pending), clear the field, and track delivery honestly.
    func send() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !commandsPaused, let sender else { return }
        let localId = Wire.ItemId("local-\(Self.localToken())")
        let issuedAt = now()
        let handle = sender.send(.reply(sessionId: sessionId, text: text), reusingId: nil)
        outgoing.append(OutgoingMessage(
            localId: localId, text: text, issuedAtMs: issuedAt,
            commandId: handle.commandId,
            state: ChatSendLogic.outgoingState(for: handle.delivery)))
        commandToOutgoing[handle.commandId] = localId
        draft = ""
        track(handle)
    }

    /// Retry a failed outgoing message. Reuses the original command id when the
    /// failure was a transport timeout/drop (dedup-safe), else mints a new id
    /// (after a definitive desktop rejection). See `ChatSendLogic.retryReusesId`.
    func retryOutgoing(_ localId: Wire.ItemId) {
        guard !commandsPaused, let sender,
              let idx = outgoing.firstIndex(where: { $0.localId == localId }),
              case let .failed(_, reusesId) = outgoing[idx].state else { return }
        let msg = outgoing[idx]
        let reuseId: Wire.CommandId? = reusesId ? msg.commandId : nil
        let handle = sender.send(.reply(sessionId: sessionId, text: msg.text), reusingId: reuseId)
        commandToOutgoing[msg.commandId] = nil
        commandToOutgoing[handle.commandId] = localId
        outgoing[idx].commandId = handle.commandId
        outgoing[idx].state = .sending
        track(handle)
    }

    // MARK: - Permission decisions

    /// Resolve a permission prompt inline (Allow once / Deny). No-op unless the
    /// prompt is the current pending one and the link is up.
    func decidePermission(promptId: Wire.PromptId, choice: Wire.PermissionChoice) {
        guard isPermissionActionable(promptId), let sender else { return }
        let handle = sender.send(
            .permissionDecision(sessionId: sessionId, promptId: promptId, choice: choice),
            reusingId: nil)
        permissionChoice[promptId] = choice
        permissionCommandId[promptId] = handle.commandId
        commandToPrompt[handle.commandId] = promptId
        permissionActions[promptId] = .sending(choice)
        track(handle)
    }

    /// Retry a failed permission decision (same id-reuse rules as replies).
    func retryPermission(_ promptId: Wire.PromptId) {
        guard !commandsPaused, let sender,
              case let .failed(_, choice, reusesId) = permissionActionState(promptId) else { return }
        let oldId = permissionCommandId[promptId]
        let reuseId: Wire.CommandId? = reusesId ? oldId : nil
        let handle = sender.send(
            .permissionDecision(sessionId: sessionId, promptId: promptId, choice: choice),
            reusingId: reuseId)
        if let oldId { commandToPrompt[oldId] = nil }
        permissionChoice[promptId] = choice
        permissionCommandId[promptId] = handle.commandId
        commandToPrompt[handle.commandId] = promptId
        permissionActions[promptId] = .sending(choice)
        track(handle)
    }

    // MARK: - Delivery reconciliation

    /// Fold a command's delivery state into the matching outgoing message or
    /// permission action. Exposed (not private) so the send state machine can be
    /// unit-tested without a live handle-observation loop.
    func applyDelivery(commandId: Wire.CommandId, state: CommandDeliveryState) {
        if let localId = commandToOutgoing[commandId],
           let idx = outgoing.firstIndex(where: { $0.localId == localId }) {
            outgoing[idx].state = ChatSendLogic.outgoingState(for: state)
        }
        if let promptId = commandToPrompt[commandId] {
            let choice = permissionChoice[promptId] ?? .allowOnce
            permissionActions[promptId] = ChatSendLogic.permissionState(for: state, choice: choice)
        }
    }

    /// Observe a handle's `delivery` and fold each change back into state. Reads
    /// the current value immediately, then re-arms on every subsequent change
    /// (Observation fires once per change).
    private func track(_ handle: CommandHandle) {
        applyDelivery(commandId: handle.commandId, state: handle.delivery)
        withObservationTracking {
            _ = handle.delivery
        } onChange: { [weak self, weak handle] in
            Task { @MainActor in
                guard let self, let handle else { return }
                self.applyDelivery(commandId: handle.commandId, state: handle.delivery)
                self.track(handle)
            }
        }
    }

    private static func localToken() -> String {
        UUID().uuidString.replacingOccurrences(of: "-", with: "").prefix(10).lowercased()
    }

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
