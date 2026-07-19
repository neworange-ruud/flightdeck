//
//  TransportStore.swift
//  FlightDeckRemote
//
//  The app-facing, `@Observable` facade over `TransportClient`. It runs on the
//  main actor and folds the transport's event stream into UI-bindable state:
//  the link state + latency, the current `StateSnapshot` (updated in place by
//  incremental `status_update` / `rollup` deltas), per-session transcript
//  accumulations (replace + append), the AgentEvents stream (deduped by
//  `event_id`), and per-command delivery-honesty handles.
//
//  It is deliberately UI-agnostic — screens bind to it next. It owns the actor
//  client and bridges its `@Sendable` events onto the main actor.
//

import Foundation
import Observation

/// A live handle to a command the phone sent. `delivery` tracks the honesty
/// state (PRD §5.8): `.sending` until the desktop acks, then `.delivered` or
/// `.failed` (the UI renders "not delivered — retry" on failure).
@MainActor
@Observable
final class CommandHandle: Identifiable {
    let commandId: Wire.CommandId
    let body: Wire.CommandBody
    var delivery: CommandDeliveryState
    /// Verbatim human-readable detail from the desktop's `command_ack`
    /// (the exact reason for a reject/fail, or a result note such as
    /// "Stopping agent…"). Additive: set by `TransportStore` when the ack
    /// carries one; surfaces show it verbatim (PRD §5.6/§5.8 honesty).
    var ackMessage: String?

    var id: String { commandId.rawValue }

    init(commandId: Wire.CommandId, body: Wire.CommandBody, delivery: CommandDeliveryState = .sending) {
        self.commandId = commandId
        self.body = body
        self.delivery = delivery
    }
}

@MainActor
@Observable
final class TransportStore {

    // MARK: - Observable state

    /// The relay link state.
    private(set) var linkState: RemoteLinkState = .disconnected
    /// Last measured phone↔relay round-trip (0 until the first pong).
    private(set) var latencyMs: Int = 0
    /// Whether the peer (desktop) is currently present (nil = unknown).
    private(set) var peerConnected: Bool?
    /// The desktop's most recently announced machine name for this pairing
    /// (REMOTE_PROTOCOL §5.7, remote-control-b8d.9) — already sanitized/
    /// bounded by `TransportClient`. `nil` until the first post-auth
    /// `machine_name` frame arrives. Consumers write this back into
    /// `PairedInstance.machineNameFromDesktop` via `PairingStore.setMachineName`
    /// (see `TransportCoordinator`'s machine-name write-back).
    private(set) var machineName: String?

    /// The current full state, folded from `snapshot` + incremental deltas.
    private(set) var snapshot: Wire.StateSnapshot?
    /// Per-session accumulated transcript items (in order).
    private(set) var transcripts: [Wire.SessionId: [Wire.TranscriptItem]] = [:]
    /// Latest full git status per session.
    private(set) var gitStatus: [Wire.SessionId: Wire.GitStatusDetail] = [:]
    /// The Activity feed / notification stream, deduped by `event_id`.
    private(set) var agentEvents: [Wire.AgentEvent] = []
    /// Per-shell accumulated output chunks (for the terminal surface later).
    private(set) var shellOutput: [Wire.ShellId: [Wire.ShellOutput]] = [:]
    /// Per-session shell lifecycle events, in arrival order (the Shell surface
    /// reads these to drive its open/exited/closed state machine). Additive,
    /// see the "Shell surface (additive)" block below.
    private(set) var shellEvents: [Wire.SessionId: [Wire.ShellEvent]] = [:]

    /// Live command handles, keyed by command id.
    private(set) var commandHandles: [Wire.CommandId: CommandHandle] = [:]

    /// Whether `snapshot`/`transcripts` currently reflect cache-seeded,
    /// offline "last-known state" (PRD §9.2) rather than a live desktop feed.
    /// Set by `seedFromCache` at launch, before the transport connects;
    /// cleared the moment a real `snapshot` message actually arrives from the
    /// desktop — that's the definitive "we're live now" signal. Screens
    /// combine this with `linkState` to decide whether to show a "showing
    /// last-known state — offline" note (see `StaleBanner`,
    /// Features/Activity).
    private(set) var isCacheStale = false

    // MARK: - Dependencies

    private let client: TransportClient
    /// Additive, optional: when set, persists a debounced last-known-state
    /// snapshot on every meaningful update (PRD §9.2 offline cache). `nil` in
    /// every existing/unit-test construction, so this never writes to disk
    /// unless a real `TransportStoreFactory`-built store explicitly supplies
    /// one.
    private let cache: SnapshotCache?
    private let now: @Sendable () -> Int64
    private var started = false
    private var seenEventIds: Set<Wire.EventId> = []

    /// The serial loop draining the transport's events onto the main actor in
    /// FIFO order, and the continuation the `@Sendable` sink feeds. See `start()`.
    private var eventLoop: Task<Void, Never>?
    private var eventContinuation: AsyncStream<TransportEvent>.Continuation?

    init(
        client: TransportClient,
        cache: SnapshotCache? = nil,
        now: @escaping @Sendable () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }
    ) {
        self.client = client
        self.cache = cache
        self.now = now
    }

    // MARK: - Lifecycle

    /// Wire the event bridge and start the transport. Idempotent.
    ///
    /// Events cross from the transport (an actor, emitting in order) to the main
    /// actor through a single FIFO `AsyncStream` drained by one serial loop.
    /// The previous bridge wrapped each event in its own `Task { @MainActor … }`,
    /// which gave no cross-event ordering guarantee — a newer `linkState` or
    /// delta could be applied before an older one (remote-control-qbj). One
    /// ordered stream + one consumer keeps delivery strictly in emit order.
    func start() async {
        guard !started else { return }
        started = true
        let (stream, continuation) = AsyncStream.makeStream(of: TransportEvent.self)
        eventContinuation = continuation
        let sink: @Sendable (TransportEvent) -> Void = { event in
            continuation.yield(event)
        }
        eventLoop = Task { @MainActor [weak self] in
            for await event in stream {
                self?.apply(event)
            }
        }
        await client.setEventHandler(sink)
        await client.start()
    }

    /// Stop the transport and tear down the event bridge.
    func stop() async {
        await client.stop()
        // Finish so the loop drains any already-buffered events before exiting;
        // cancel is a backstop.
        eventContinuation?.finish()
        eventContinuation = nil
        eventLoop?.cancel()
        eventLoop = nil
        started = false
    }

    // MARK: - Command API

    /// Seal and send a command; returns a live `CommandHandle` whose `delivery`
    /// state updates as the desktop acks (or the honesty timeout fires).
    @discardableResult
    func sendCommand(_ body: Wire.CommandBody) -> CommandHandle {
        sendCommand(body, commandId: Wire.CommandId("cmd_\(Self.token())"))
    }

    /// Seal and send a command under an explicit `commandId`. Reusing a prior
    /// id makes a retry idempotent: the desktop dedups by `command_id` (PRD
    /// §5.8), so a command that may already have applied is never double-applied.
    /// Callers that don't need id reuse should use `sendCommand(_:)`.
    @discardableResult
    func sendCommand(_ body: Wire.CommandBody, commandId: Wire.CommandId) -> CommandHandle {
        let handle = CommandHandle(commandId: commandId, body: body)
        commandHandles[commandId] = handle
        let command = Wire.PhoneCommand(commandId: commandId, issuedAtMs: now(), body: body)
        Task { await client.send(command) }
        return handle
    }

    /// Ask the desktop for a session's transcript (all of it, or from an index).
    @discardableResult
    func requestTranscript(_ sessionId: Wire.SessionId, fromIndex: UInt64? = nil) -> CommandHandle {
        sendCommand(.requestTranscript(sessionId: sessionId, fromIndex: fromIndex))
    }

    /// Ask for a fresh snapshot (all projects, or one).
    @discardableResult
    func requestSnapshot(projectId: Wire.ProjectId? = nil) -> CommandHandle {
        sendCommand(.requestSnapshot(projectId: projectId))
    }

    // MARK: - Push-token registration (PRD §9.1, spec §5.5)

    /// Hand the APNs device token (from `AppDelegate`) to the transport so it
    /// registers it with the relay for this pairing. Safe to call before the
    /// link is live and repeatedly (e.g. on token refresh): the client caches
    /// it and (re-)sends on each `auth_ok`. Not a sealed command — the token is
    /// opaque and travels on the relay plane (outside E2E).
    func registerPushToken(_ token: String, environment: Wire.ApnsEnvironment) {
        Task { await client.registerPushToken(token, environment: environment) }
    }

    // MARK: - Shell surface (additive)
    //
    // Thin wrappers over `sendCommand` for the minimal shell terminal (PRD
    // §5.4). They exist so the Shell feature talks to the transport through
    // one named surface rather than hand-building `CommandBody.shell*` cases,
    // and so `TransportStore` can satisfy `ShellCommandSending`. Output chunks
    // land in `shellOutput` and lifecycle events in `shellEvents` (above).

    /// Open a shell in the session's worktree with a fitted geometry.
    @discardableResult
    func openShell(sessionId: Wire.SessionId, shellId: Wire.ShellId,
                   cols: UInt16, rows: UInt16) -> CommandHandle {
        sendCommand(.shellOpen(sessionId: sessionId, shellId: shellId, cols: cols, rows: rows))
    }

    /// Send input (keystrokes/text, already encoded to the wire string) to a
    /// live shell.
    @discardableResult
    func sendShellInput(sessionId: Wire.SessionId, shellId: Wire.ShellId,
                        data: String) -> CommandHandle {
        sendCommand(.shellInput(sessionId: sessionId, shellId: shellId, data: data))
    }

    /// Interrupt the shell's foreground process (Ctrl-C). Uses the protocol
    /// command rather than a raw `0x03` byte so it works even when the PTY is
    /// wedged (PRD §5.4 interrupt).
    @discardableResult
    func interruptShell(sessionId: Wire.SessionId, shellId: Wire.ShellId) -> CommandHandle {
        sendCommand(.shellInterrupt(sessionId: sessionId, shellId: shellId))
    }

    /// Close the shell (releases the desktop-held slot).
    @discardableResult
    func closeShell(sessionId: Wire.SessionId, shellId: Wire.ShellId) -> CommandHandle {
        sendCommand(.shellClose(sessionId: sessionId, shellId: shellId))
    }

    // MARK: - Event folding

    private func apply(_ event: TransportEvent) {
        switch event {
        case let .link(state):
            linkState = state
            if case let .connected(latency) = state { latencyMs = latency }
        case let .presence(_, connected):
            peerConnected = connected
        case let .machineName(name):
            machineName = name
        case let .delivery(commandId, state):
            commandHandles[commandId]?.delivery = state
        case let .message(message):
            fold(message)
        }
    }

    private func fold(_ message: Wire.DesktopToPhone) {
        switch message {
        case let .snapshot(snap):
            snapshot = snap
            // A full snapshot from the desktop is the definitive "we're live
            // now" signal — clears any cache-seeded staleness (PRD §9.2).
            isCacheStale = false
            cacheCurrentState()
        case let .statusUpdate(update):
            applyStatusUpdate(update)
            cacheCurrentState()
        case let .rollup(update):
            applyRollup(update)
            cacheCurrentState()
        case let .transcript(feed):
            applyTranscript(feed, replace: true)
            cacheCurrentState()
        case let .transcriptAppend(feed):
            applyTranscript(feed, replace: false)
            cacheCurrentState()
        case let .event(agentEvent):
            if seenEventIds.insert(agentEvent.eventId).inserted {
                agentEvents.append(agentEvent)
            }
        case let .gitStatus(detail):
            gitStatus[detail.sessionId] = detail
        case let .shellOutput(output):
            shellOutput[output.shellId, default: []].append(output)
        case let .shellEvent(event):
            shellEvents[event.sessionId, default: []].append(event)
        case let .commandAck(ack):
            // Delivery honesty is driven by the transport's delivery events;
            // the ack's human-readable message (verbatim reject/fail reason or
            // result note) is kept on the handle for honest display.
            if let message = ack.message {
                commandHandles[ack.commandId]?.ackMessage = message
            }
        }
    }

    private func applyStatusUpdate(_ update: Wire.StatusUpdate) {
        guard var snap = snapshot else { return }
        for delta in update.updates {
            guard let pIdx = snap.projects.firstIndex(where: { $0.projectId == delta.projectId }),
                  let sIdx = snap.projects[pIdx].sessions.firstIndex(where: { $0.sessionId == delta.sessionId })
            else { continue }
            snap.projects[pIdx].sessions[sIdx].status = delta.status
            if let running = delta.runningTimeSecs {
                snap.projects[pIdx].sessions[sIdx].runningTimeSecs = running
            }
            if let question = delta.pendingQuestion {
                snap.projects[pIdx].sessions[sIdx].pendingQuestion = question
            }
        }
        snapshot = snap
    }

    private func applyRollup(_ update: Wire.RollupUpdate) {
        guard var snap = snapshot else { return }
        for refresh in update.projects {
            guard let pIdx = snap.projects.firstIndex(where: { $0.projectId == refresh.projectId })
            else { continue }
            snap.projects[pIdx].rollup = refresh.rollup
        }
        snapshot = snap
    }

    private func applyTranscript(_ feed: Wire.TranscriptFeed, replace: Bool) {
        let from = Int(feed.fromIndex)
        if replace {
            var items = transcripts[feed.sessionId] ?? []
            if from <= items.count {
                items.removeSubrange(from..<items.count)
            }
            items.append(contentsOf: feed.items)
            transcripts[feed.sessionId] = items
        } else {
            transcripts[feed.sessionId, default: []].append(contentsOf: feed.items)
        }
    }

    // MARK: - Offline cache (PRD §9.2)

    /// Seeds `snapshot`/`transcripts` from a previously-cached state and
    /// marks it stale. Additive hook called by `TransportStoreFactory` before
    /// `start()` — never by `TransportClient` itself, so this can never race
    /// a live snapshot arriving.
    func seedFromCache(_ cached: SnapshotCache.CachedState) {
        snapshot = cached.snapshot
        for entry in cached.transcripts {
            transcripts[entry.sessionId] = entry.items
        }
        isCacheStale = true
    }

    /// Schedules a debounced cache write of the current `snapshot`/`transcripts`,
    /// a no-op absent both a snapshot and a configured `cache`.
    private func cacheCurrentState() {
        guard let cache, let snapshot else { return }
        cache.scheduleSave(snapshot: snapshot, transcripts: transcripts)
    }

    // MARK: - Helpers

    private static func token() -> String {
        UUID().uuidString.replacingOccurrences(of: "-", with: "").prefix(12).lowercased()
    }
}

#if DEBUG
extension TransportStore {
    /// DEBUG-only seam: force-set `snapshot` (and optionally `linkState`)
    /// directly, bypassing the real `TransportClient`/relay entirely.
    ///
    /// Exists so the Projects/Sessions screens can render deterministically
    /// without a live desktop: `TransportStoreFactory` calls this to seed a
    /// realistic fixture snapshot when the app launches with the
    /// `-uitest-fixture-snapshot` argument (UI tests, scripted screenshots),
    /// and SwiftUI previews can call it directly on a store built with a
    /// never-started client. Compiled out of Release builds — there is no
    /// way to reach this from production code.
    func debugSeed(
        snapshot: Wire.StateSnapshot,
        linkState: RemoteLinkState = .connected(latencyMs: 8)
    ) {
        self.snapshot = snapshot
        self.linkState = linkState
    }
}
#endif
