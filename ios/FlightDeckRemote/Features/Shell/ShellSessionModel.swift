//
//  ShellSessionModel.swift
//  FlightDeckRemote
//
//  The `@Observable` brain behind the shell surface (PRD §5.4). It is the one
//  place the pure pieces meet the transport:
//   - `ShellStateMachine` drives `phase` (connect → live → exited → closed,
//     plus the "already open" rejection);
//   - `ShellOutputBuffer` reassembles streamed `ShellOutput` chunks in seq
//     order (gap-tolerant) into `orderedOutput`, which the renderer feeds;
//   - `ShellKeyBarLogic` / `ShellByteEncoding` turn key-bar taps and keyboard
//     input into `shell_input` bytes, applying the sticky-`Ctrl` modifier;
//   - `CommandsPausedGate` gates every send (PRD §8: nothing sent blind).
//
//  It does NOT observe the store itself — the view reads the relevant store
//  state in its `body` (so Observation tracks it) and calls `ingest*` on
//  change. That keeps the fragile "feed new bytes to a UIKit view" path
//  driven by real SwiftUI invalidation rather than a manual subscription.
//
//  Live vs. fixture: bound to a real `TransportStore` (`configure`), sends go
//  to the relay and output/events stream from the store. In the DEBUG fixture
//  / UI-test path a `ScriptedShellCommandSender` records sends and
//  `debugDriveFixture` seeds a live shell + scripted output, with no relay.
//

import Foundation
import Observation

@MainActor
@Observable
final class ShellSessionModel {

    // MARK: - Identity

    let sessionId: Wire.SessionId
    let sessionName: String

    // MARK: - Observable surface state

    /// The lifecycle phase (drives the CTA / live terminal / exit banner).
    private(set) var phase: ShellPhase = .noShell
    /// The active shell id (minted on open; the desktop echoes it back).
    private(set) var shellId: Wire.ShellId?
    /// Sticky `Ctrl`: armed until the next key folds into a control byte.
    private(set) var ctrlArmed = false
    /// Reassembled output chunks in seq order — the renderer feeds new tail.
    private(set) var orderedOutput: [String] = []

    #if DEBUG
    /// Bumped on every send (input / interrupt) so the DEBUG `shell-debug-last-sent`
    /// UI-test label refreshes even for sends that change no other observable
    /// state (the scripted sender itself isn't `@Observable`).
    private(set) var debugSendVersion = 0
    #endif

    /// `true` while the link isn't live — every send is blocked and the UI
    /// shows an honest paused note (PRD §8).
    var commandsPaused: Bool { gate?.commandsPaused ?? true }

    /// Fitted terminal geometry, set by the renderer before `open()`.
    private(set) var cols: UInt16 = 80
    private(set) var rows: UInt16 = 24

    /// The open command's live delivery state, exposed so the view can observe
    /// it (reading a `CommandHandle`'s `delivery` in `body`) and trigger
    /// `reconcileOpenDelivery` on change (drives the "already open" rejection).
    var openHandleDelivery: CommandDeliveryState? { openHandle?.delivery }

    // MARK: - Dependencies

    private var sender: (any ShellCommandSending)?
    private var gate: CommandsPausedGate?
    private weak var store: TransportStore?

    // MARK: - Internal reassembly / cursors

    private var buffer = ShellOutputBuffer()
    private var openHandle: CommandHandle?
    private var consumedOutput = 0
    private var consumedEvents = 0
    private let idFactory: () -> Wire.ShellId

    init(sessionId: Wire.SessionId,
         sessionName: String,
         idFactory: @escaping () -> Wire.ShellId = { Wire.ShellId("sh_\(UUID().uuidString.prefix(8).lowercased())") }) {
        self.sessionId = sessionId
        self.sessionName = sessionName
        self.idFactory = idFactory
    }

    // MARK: - Configuration

    /// Wire the live transport: the store is both the command sender and the
    /// paused-gate source, and the origin of output/events the view ingests.
    func configure(store: TransportStore) {
        self.store = store
        self.sender = store
        self.gate = CommandsPausedGate(source: store)
    }

    /// Wire an explicit sender + gate (DEBUG fixture / unit tests).
    func configure(sender: any ShellCommandSending, gate: CommandsPausedGate) {
        self.sender = sender
        self.gate = gate
    }

    /// Record the fitted geometry from the renderer. Only meaningful before
    /// `open()`; a live resize is a separate (deferred) issue.
    func setGeometry(cols: UInt16, rows: UInt16) {
        guard cols > 0, rows > 0 else { return }
        self.cols = cols
        self.rows = rows
    }

    // MARK: - Lifecycle actions

    /// Open (or reopen) the shell. Mints a fresh `shellId`, resets reassembly,
    /// and sends `shell_open` with the fitted geometry.
    func open() {
        guard !commandsPaused else { return }
        let id = idFactory()
        shellId = id
        phase = .opening
        buffer = ShellOutputBuffer()
        orderedOutput = []
        consumedOutput = 0
        // Do not reset `consumedEvents`: the store's per-session event log is
        // append-only, so we keep advancing past events we've already seen and
        // only react to ones for this new `shellId`.
        openHandle = sender?.openShell(sessionId: sessionId, shellId: id, cols: cols, rows: rows)
    }

    /// Alias used by the exit/rejection banners' "Reopen" affordance.
    func reopen() { open() }

    /// Close the shell (releases the desktop slot). Safe from any phase.
    func close() {
        if let id = shellId {
            sender?.closeShell(sessionId: sessionId, shellId: id)
        }
        phase = .closed
        ctrlArmed = false
    }

    // MARK: - Input

    /// A key-bar tap (PRD §5.4 accessory bar).
    func tapKey(_ key: ShellKey) {
        switch ShellKeyBarLogic.action(for: key, ctrlArmed: ctrlArmed) {
        case .toggleCtrl:
            ctrlArmed.toggle()
        case .interrupt:
            interrupt()
        case .paste:
            pasteFromClipboard()
        case let .sendBytes(bytes, ctrlConsumed):
            if ctrlConsumed { ctrlArmed = false }
            sendInput(bytes)
        }
    }

    /// Raw keyboard input from the terminal view's own keyboard. Applies the
    /// sticky-`Ctrl` modifier (so arming `Ctrl` then typing `c` sends `0x03`).
    func handleKeyboardInput(_ bytes: [UInt8]) {
        let resolved = ShellKeyBarLogic.keyboardInput(bytes, ctrlArmed: ctrlArmed)
        if resolved.ctrlConsumed { ctrlArmed = false }
        sendInput(resolved.bytes)
    }

    /// Interrupt the foreground process. Sends the `shell_interrupt` *command*
    /// (not a raw `0x03`) so it lands even when the PTY is wedged (PRD §5.4).
    func interrupt() {
        guard !commandsPaused, let id = shellId, isInteractive else { return }
        ctrlArmed = false
        sender?.interruptShell(sessionId: sessionId, shellId: id)
        #if DEBUG
        debugSendVersion &+= 1
        #endif
    }

    /// Paste: read `UIPasteboard` text and send it as input (plain, v1 — see
    /// `ShellByteEncoding` on bracketed paste). The view supplies the string so
    /// this stays UIKit-free and testable.
    func paste(_ text: String) {
        guard !text.isEmpty else { return }
        sendInput(ShellByteEncoding.bytes(for: text))
    }

    /// Toggle sticky `Ctrl` (also reachable via the key-bar tap).
    func toggleCtrl() { ctrlArmed.toggle() }

    // MARK: - Store ingestion (called by the view on store change)

    /// Fold any new lifecycle events for this session into `phase`.
    func ingestStoreEvents() {
        guard let events = store?.shellEvents[sessionId] else { return }
        while consumedEvents < events.count {
            let event = events[consumedEvents]
            consumedEvents += 1
            // Only react to events for our current shell (ignore a stale
            // prior shell's tail after a reopen).
            guard event.shellId == shellId else { continue }
            phase = ShellStateMachine.reduce(phase, ShellStateMachine.input(for: event.kind))
        }
    }

    /// Ingest any new output chunks for the active shell, reassembled in seq
    /// order, appending the emitted tail to `orderedOutput`.
    func ingestStoreOutput() {
        guard let id = shellId, let chunks = store?.shellOutput[id] else { return }
        while consumedOutput < chunks.count {
            let chunk = chunks[consumedOutput]
            consumedOutput += 1
            let emitted = buffer.ingest(seq: chunk.seq, data: chunk.data)
            if !emitted.isEmpty { orderedOutput.append(contentsOf: emitted) }
        }
    }

    /// Reconcile the open command's delivery: a `.rejected` ack means a shell
    /// is already open (desktop contract) — surface it honestly.
    func reconcileOpenDelivery() {
        guard case .opening = phase, let delivery = openHandle?.delivery else { return }
        if case .delivered(.rejected) = delivery {
            // Prefer the desktop's verbatim reason (e.g. "already open") when
            // the ack carried one; otherwise an honest default.
            phase = .rejectedAlreadyOpen(
                message: openHandle?.ackMessage
                    ?? "A shell is already open for this session on the desktop.")
        }
    }

    // MARK: - Private

    /// Whether input/interrupt make sense right now (live shell only).
    private var isInteractive: Bool {
        if case .live = phase { return true }
        return false
    }

    private func sendInput(_ bytes: [UInt8]) {
        guard !commandsPaused, isInteractive, !bytes.isEmpty, let id = shellId else { return }
        sender?.sendShellInput(sessionId: sessionId, shellId: id,
                               data: ShellByteEncoding.wireString(bytes))
        #if DEBUG
        debugSendVersion &+= 1
        #endif
    }

    /// Overridden by the view to read the real pasteboard; the default is a
    /// no-op so the model stays UIKit-free and unit-testable.
    var pasteProvider: () -> String? = { nil }
    private func pasteFromClipboard() {
        if let text = pasteProvider(), !text.isEmpty { paste(text) }
    }
}

#if DEBUG
extension ShellSessionModel {
    /// DEBUG seam: drive a live shell with scripted output, no relay. Used by
    /// the `-uitest-fixture-shell` path and previews. Feeds the chunks through
    /// the real buffer (so seq/ANSI handling is exercised), and flips `phase`
    /// to `.live`.
    func debugDriveFixture(shellId id: Wire.ShellId = Wire.ShellId("sh_fixture"),
                           chunks: [String]) {
        shellId = id
        phase = .live
        buffer = ShellOutputBuffer()
        orderedOutput = []
        for (i, data) in chunks.enumerated() {
            let emitted = buffer.ingest(seq: UInt64(i + 1), data: data)
            orderedOutput.append(contentsOf: emitted)
        }
    }

    /// DEBUG accessor for the scripted sender's last-send description (drives
    /// the hidden `shell-debug-last-sent` UI-test label). Reads `debugSendVersion`
    /// first so the label refreshes on every send (see that property).
    var debugLastSentDescription: String {
        _ = debugSendVersion
        return (sender as? ScriptedShellCommandSender)?.lastSentDescription ?? "none"
    }
}
#endif
