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

    /// Live command handles, keyed by command id.
    private(set) var commandHandles: [Wire.CommandId: CommandHandle] = [:]

    // MARK: - Dependencies

    private let client: TransportClient
    private let now: @Sendable () -> Int64
    private var started = false
    private var seenEventIds: Set<Wire.EventId> = []

    init(
        client: TransportClient,
        now: @escaping @Sendable () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }
    ) {
        self.client = client
        self.now = now
    }

    // MARK: - Lifecycle

    /// Wire the event bridge and start the transport. Idempotent.
    func start() async {
        guard !started else { return }
        started = true
        let sink: @Sendable (TransportEvent) -> Void = { [weak self] event in
            Task { @MainActor in self?.apply(event) }
        }
        await client.setEventHandler(sink)
        await client.start()
    }

    /// Stop the transport.
    func stop() async {
        await client.stop()
        started = false
    }

    // MARK: - Command API

    /// Seal and send a command; returns a live `CommandHandle` whose `delivery`
    /// state updates as the desktop acks (or the honesty timeout fires).
    @discardableResult
    func sendCommand(_ body: Wire.CommandBody) -> CommandHandle {
        let commandId = Wire.CommandId("cmd_\(Self.token())")
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

    // MARK: - Event folding

    private func apply(_ event: TransportEvent) {
        switch event {
        case let .link(state):
            linkState = state
            if case let .connected(latency) = state { latencyMs = latency }
        case let .presence(_, connected):
            peerConnected = connected
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
        case let .statusUpdate(update):
            applyStatusUpdate(update)
        case let .rollup(update):
            applyRollup(update)
        case let .transcript(feed):
            applyTranscript(feed, replace: true)
        case let .transcriptAppend(feed):
            applyTranscript(feed, replace: false)
        case let .event(agentEvent):
            if seenEventIds.insert(agentEvent.eventId).inserted {
                agentEvents.append(agentEvent)
            }
        case let .gitStatus(detail):
            gitStatus[detail.sessionId] = detail
        case let .shellOutput(output):
            shellOutput[output.shellId, default: []].append(output)
        case .shellEvent:
            break // lifecycle handled by the terminal surface later
        case .commandAck:
            break // delivery honesty is driven by the transport's delivery events
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
