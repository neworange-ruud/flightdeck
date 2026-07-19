//
//  TransportClient.swift
//  FlightDeckRemote
//
//  The relay-plane state machine (REMOTE_PROTOCOL §5/§6), as an actor. It is
//  the phone-side counterpart to the desktop's `src/remote/client.rs` and
//  mirrors its semantics:
//
//    connect → hello → hello_ok → auth_challenge → auth_response → auth_ok
//            → resume (from the persisted inbound cursor) + request_snapshot
//            → pump: read inbound / seal & send outbound / periodic ping
//
//  Responsibilities:
//   - Auth: sign the relay's nonce with `DeviceIdentity` (ECDSA P-256), §5.1.
//   - Inbound envelopes: dedup by `seq` per (pairing, sender=desktop), open via
//     `E2EChannel`, decode `DesktopToPhone`, publish, then cumulatively `ack`
//     durable receipt (§6.2/§6.4). A failed open/AAD-mismatch is rejected and
//     never advances the cursor (§7.1).
//   - Outbound commands: assign a gapless `seq` from the persisted cursor, seal
//     the `PhoneCommand` with `E2EChannel`, send the envelope, and persist the
//     cursor only after a successful send (§6.1). Delivery honesty (§6.5): a
//     command is `.sending` until the desktop's `command_ack`; a ~10s timeout
//     (or a send failure / peer-down) flips it to `.failed`.
//   - Reconnect: exponential backoff + jitter on any drop (`Backoff`, §5.3); a
//     session that reached `auth_ok` resets the attempt counter.
//   - Latency: a relay-plane `ping` every ~20s; `pong` updates `connected`.
//
//  It publishes `TransportEvent`s through a `@Sendable` handler that
//  `TransportStore` sets to fold state onto the main actor. The state machine
//  is fully UI-agnostic and driven by an injected `WebSocketConnecting`, so
//  unit tests run it against a scripted mock socket.
//

import Foundation
import os

/// Opt-in diagnostic logger for the desktop→phone receive path (the
/// desktop→phone delivery investigation). View live in Xcode's console, or in
/// Console.app filtered by subsystem `agency.neworange.flightdeck.remote`,
/// category `transport-diag`. Search for the `FDDIAG` prefix. Metadata only —
/// on a decode failure it also dumps the plaintext JSON so the exact wire
/// mismatch is visible; that is the user's own payload on their own device.
let transportDiag = Logger(
    subsystem: "agency.neworange.flightdeck.remote",
    category: "transport-diag"
)

actor TransportClient {

    // MARK: - Tuning

    struct Config: Sendable {
        /// Latency-probe interval.
        var pingInterval: Duration = .seconds(20)
        /// Delivery-honesty deadline: a command with no `command_ack` by this
        /// point is reported `.failed` (PRD §5.8).
        var commandTimeout: Duration = .seconds(10)
        /// Whether to request a fresh snapshot right after `resume`.
        var requestSnapshotOnResume: Bool = true

        init() {}
    }

    // MARK: - Dependencies

    private let identity: DeviceIdentity
    private let keyAgreement: KeyAgreementKeys
    private let recordStore: PairingRecordStore
    /// The specific pairing this client drives, when the (multi-pairing)
    /// `TransportCoordinator` instantiates one client per `PairedInstance`
    /// (remote-control-b8d.5). `nil` keeps the transitional single-pairing
    /// behavior — the client binds to the first stored record — so existing
    /// single-store call sites and their tests are unchanged. When set, the
    /// client loads and persists cursors against *only* that pairing's keyed
    /// record, so N clients sharing one `PairingRecordStore` never rewrite each
    /// other's watermarks.
    private let targetPairingId: String?
    private let connector: any WebSocketConnecting
    private let clientInfo: Wire.ClientInfo
    private let config: Config
    private let jitter: @Sendable () -> Double
    private let now: @Sendable () -> Int64

    // MARK: - Runtime state

    private var eventHandler: (@Sendable (TransportEvent) -> Void)?
    private var supervisor: Task<Void, Never>?

    private var record: PairingRecord?
    private var channel: (any WebSocketChannel)?
    private var e2e: E2EChannel?
    private var pinger: Task<Void, Never>?

    private var linkState: RemoteLinkState = .disconnected
    private var latencyMs: Int = 0
    private var phase: Phase = .idle
    private var sessionAuthed = false
    private var peerConnected: Bool?

    /// The latest APNs push token to register for this pairing (spec §5.5).
    /// Held so it can be (re)sent the moment the session reaches `auth_ok` —
    /// the token typically arrives from `AppDelegate` before the transport is
    /// live, and must be re-sent after every reconnect since the relay's v1
    /// in-memory store drops it on restart. `nil` until the app hands one over.
    private var pushToken: (token: String, environment: Wire.ApnsEnvironment)?

    /// Whether push from THIS pairing is muted (per-machine mute,
    /// remote-control-b8d.10). While muted the client never registers this
    /// pairing's token, and if it becomes muted mid-session it actively sends
    /// `unregister_push_token` to drop whatever the relay is holding. Muting one
    /// pairing never touches another — each `TransportClient` owns its own flag.
    private var pushMuted = false

    /// The token this client has registered with the relay in the CURRENT live
    /// session, or `nil` if it has registered nothing (fresh session, muted, or
    /// just deregistered). Guards against double-registering the same token
    /// while live, yet resets on every session teardown so `auth_ok` always
    /// re-registers after a reconnect (the relay's v1 in-memory store may have
    /// dropped it across a restart — spec §5.5 / store.rs module docs).
    private var registeredToken: String?

    private var pending: [Wire.CommandId: PendingCommand] = [:]

    /// The pre-`auth_ok` handshake phase within one session.
    private enum Phase: Equatable {
        case idle
        case awaitingHelloOk
        case awaitingChallenge
        case awaitingAuthOk
        case live
    }

    private struct PendingCommand {
        let seq: UInt64
        let timeout: Task<Void, Never>
    }

    // MARK: - Init

    init(
        identity: DeviceIdentity,
        keyAgreement: KeyAgreementKeys,
        recordStore: PairingRecordStore,
        pairingId: String? = nil,
        connector: any WebSocketConnecting,
        clientInfo: Wire.ClientInfo? = nil,
        config: Config = Config(),
        jitter: @escaping @Sendable () -> Double = { Double.random(in: 0..<1) },
        now: @escaping @Sendable () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }
    ) {
        self.identity = identity
        self.keyAgreement = keyAgreement
        self.recordStore = recordStore
        self.targetPairingId = pairingId
        self.connector = connector
        self.clientInfo = clientInfo ?? Wire.ClientInfo(
            appVersion: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "1.0",
            platform: "ios",
            osVersion: nil
        )
        self.config = config
        self.jitter = jitter
        self.now = now
    }

    /// Set the observer that receives every `TransportEvent`.
    func setEventHandler(_ handler: @escaping @Sendable (TransportEvent) -> Void) {
        self.eventHandler = handler
    }

    // MARK: - Lifecycle

    /// Start the reconnect supervisor. No-op if already running.
    func start() {
        guard supervisor == nil else { return }
        supervisor = Task { [weak self] in
            await self?.runSupervisor()
        }
    }

    /// Stop the supervisor and tear down the current connection.
    func stop() async {
        supervisor?.cancel()
        supervisor = nil
        pinger?.cancel()
        pinger = nil
        await channel?.close()
        channel = nil
        for (_, p) in pending { p.timeout.cancel() }
        pending.removeAll()
        phase = .idle
        registeredToken = nil
        setLink(.disconnected)
    }

    /// The current link state (test/inspection convenience).
    func currentLinkState() -> RemoteLinkState { linkState }

    /// The pairing this client is bound to, when instantiated per-`PairedInstance`
    /// by the coordinator (`nil` for the transitional single-pairing wiring).
    nonisolated var pairingId: String? { targetPairingId }

    /// Load this client's record: the keyed record for `targetPairingId` when
    /// the coordinator scoped this client to one pairing, else the transitional
    /// first-record shim.
    private func loadRecord() throws -> PairingRecord? {
        if let targetPairingId {
            return try recordStore.load(pairingId: targetPairingId)
        }
        return try recordStore.load()
    }

    // MARK: - Public command API

    /// Seal and send a phone command. Delivery honesty is reported through the
    /// event stream keyed by `command.commandId`.
    func send(_ command: Wire.PhoneCommand) async {
        await sendEnvelope(for: command, track: true)
    }

    /// Register (or refresh) the APNs push token for this pairing (spec §5.5).
    /// The token is remembered and sent immediately if the session is already
    /// live, and re-sent on every subsequent `auth_ok` (reconnect). Registering
    /// the same token again is harmless — the relay just overwrites it. It is a
    /// plaintext relay-plane frame (the token is opaque and outside E2E), not a
    /// sealed command.
    func registerPushToken(_ token: String, environment: Wire.ApnsEnvironment) {
        pushToken = (token, environment)
        if phase == .live, let ch = channel {
            Task { await reconcilePushToken(on: ch) }
        }
    }

    /// Mute/unmute push from this pairing (per-machine mute, spec §5.5 /
    /// remote-control-b8d.10). Muting a *live* pairing actively deregisters its
    /// token (so the relay stops pushing immediately); unmuting re-registers the
    /// held token. When not live the flag is just remembered and applied on the
    /// next `auth_ok`. Idempotent — a no-op if the flag is unchanged.
    func setPushMuted(_ muted: Bool) {
        guard muted != pushMuted else { return }
        pushMuted = muted
        if phase == .live, let ch = channel {
            Task { await reconcilePushToken(on: ch) }
        }
    }

    /// Apply the token AND mute intent for this pairing in ONE atomic step
    /// (remote-control-b8d.10). The `TransportCoordinator` uses this so a single
    /// `reconcilePushToken` decides register-vs-unregister-vs-nothing — setting
    /// the token and the mute flag through the two granular setters instead
    /// would spawn two independent reconciles that could each emit a frame.
    /// `token == nil` leaves the held token untouched (e.g. no APNs token yet).
    func applyPush(token: (token: String, environment: Wire.ApnsEnvironment)?, muted: Bool) {
        if let token { pushToken = token }
        pushMuted = muted
        if phase == .live, let ch = channel {
            Task { await reconcilePushToken(on: ch) }
        }
    }

    // MARK: - Supervisor

    private func runSupervisor() async {
        record = (try? loadRecord()) ?? nil
        guard record != nil else {
            setLink(.disconnected)
            return
        }

        var attempt = 0
        while !Task.isCancelled {
            setLink(.connecting)
            let authed = await runSession()
            if Task.isCancelled { break }
            setLink(.disconnected)
            attempt = authed ? 0 : attempt + 1
            let delay = Backoff.delay(attempt: attempt, jitterUnit: jitter())
            try? await Task.sleep(for: delay)
        }
    }

    /// One connection session. Returns whether it reached `auth_ok` (so the
    /// supervisor can reset backoff after a genuinely-good session dropped).
    private func runSession() async -> Bool {
        guard let record else { return false }
        guard let url = URL(string: record.relayURL) else { return false }

        let ch: any WebSocketChannel
        do {
            ch = try await connector.connect(to: url)
        } catch {
            return false
        }
        channel = ch
        sessionAuthed = false
        phase = .awaitingHelloOk

        // Derive the E2E channel for this pairing up front (needed the instant a
        // replayed envelope arrives after resume).
        do {
            e2e = try deriveChannel(record)
        } catch {
            await ch.close()
            channel = nil
            return false
        }

        // hello.
        do {
            try await ch.send(.hello(
                protocolVersion: Wire.protocolVersion,
                role: .phone,
                deviceId: Wire.DeviceId(identity.deviceId),
                client: clientInfo
            ))
        } catch {
            await ch.close()
            channel = nil
            return false
        }
        setLink(.authenticating)

        startPinger(ch)

        // Single receive loop driving the handshake then steady-state pump.
        while !Task.isCancelled {
            let frame: Wire.RelayFrame
            do {
                frame = try await ch.receive()
            } catch {
                break // drop / clean close / decode failure ends the session
            }
            let keepGoing = await handle(frame, on: ch)
            if !keepGoing { break }
        }

        pinger?.cancel()
        pinger = nil
        await ch.close()
        channel = nil
        e2e = nil
        phase = .idle
        // A new session must re-register from scratch (the relay's v1 in-memory
        // store may have dropped our token), so forget what THIS session sent.
        registeredToken = nil
        return sessionAuthed
    }

    private func startPinger(_ ch: any WebSocketChannel) {
        pinger?.cancel()
        let interval = config.pingInterval
        let clock = now
        pinger = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: interval)
                if Task.isCancelled { return }
                try? await ch.send(.ping(clientTimeMs: clock()))
                _ = self // keep capture explicit
            }
        }
    }

    // MARK: - Frame handling

    /// Handle one inbound frame. Returns `false` to end the session (reconnect).
    private func handle(_ frame: Wire.RelayFrame, on ch: any WebSocketChannel) async -> Bool {
        switch frame {
        case .helloOk:
            if phase == .awaitingHelloOk { phase = .awaitingChallenge }
            return true

        case .versionIncompatible:
            return false

        case let .authChallenge(nonce, _):
            guard phase == .awaitingChallenge else { return true }
            let signature: String
            do {
                signature = try identity.signBase64(nonceBase64: nonce)
            } catch {
                return false
            }
            let pairingIds = record.map { [Wire.PairingId($0.pairingId)] } ?? []
            do {
                try await ch.send(.authResponse(
                    deviceId: Wire.DeviceId(identity.deviceId),
                    signature: signature,
                    pairingIds: pairingIds
                ))
            } catch {
                return false
            }
            phase = .awaitingAuthOk
            return true

        case .authOk:
            guard phase == .awaitingAuthOk else { return true }
            sessionAuthed = true
            phase = .live
            latencyMs = 0
            setLink(.connected(latencyMs: 0))
            transportDiag.notice("FDDIAG connection LIVE (auth_ok) pairing=\(self.record?.pairingId ?? "nil", privacy: .public)")
            await onAuthenticated(ch)
            return true

        case let .peerPresence(_, peer, state, _):
            let connected = state == .connected
            if peer == .desktop { peerConnected = connected }
            emit(.presence(peer: peer, connected: connected))
            return true

        case let .machineName(pairingId, name):
            // Defensive: only apply a name announced for OUR pairing (this
            // client is scoped to exactly one pairing/record — b8d.5).
            guard record?.pairingId == pairingId.rawValue else { return true }
            // Sanitize/bound before it ever reaches the store (§5.7): an
            // all-whitespace result reads as "no name" and is dropped rather
            // than clobbering the previous/fallback display name.
            if let sanitized = Wire.sanitizeMachineName(name) {
                emit(.machineName(sanitized))
            }
            return true

        case let .envelope(env):
            await handleEnvelope(env, on: ch)
            return true

        case let .pong(clientTimeMs, _):
            latencyMs = max(0, Int(now() - clientTimeMs))
            setLink(.connected(latencyMs: latencyMs))
            return true

        case let .error(code, _, pairingId):
            // A `seq_violation` is recoverable, not fatal: the relay lost its
            // outbound watermark for us (restart) and our command seq ran ahead.
            // Re-sync our outbound cursor so the next command restarts at seq 1
            // rather than reconnecting into the same rejection forever (bbf).
            if code == .seqViolation { await handleSeqViolation(pairingId) }
            return !isFatal(code)

        case .bye:
            return false

        // Frames the phone never receives in steady state, or handshake
        // restatements: ignore and keep the session alive.
        case .ack, .resume, .ping, .hello, .authResponse,
             .pairingOffer, .pairingOfferOk, .pairingClaim, .pairingClaimed,
             .registerPushToken, .unregisterPushToken, .pushTokenAck:
            return true
        }
    }

    /// After `auth_ok`: resume from the persisted inbound cursor, then request a
    /// fresh snapshot so the UI has current state immediately.
    private func onAuthenticated(_ ch: any WebSocketChannel) async {
        guard let record else { return }
        try? await ch.send(.resume(
            pairingId: Wire.PairingId(record.pairingId),
            fromSeq: record.lastReceivedSeq
        ))
        if config.requestSnapshotOnResume {
            let cmd = Wire.PhoneCommand(
                commandId: Wire.CommandId("cmd_snap_\(shortToken())"),
                issuedAtMs: now(),
                body: .requestSnapshot(projectId: nil)
            )
            // A read: don't surface delivery honesty for the implicit refresh.
            await sendEnvelope(for: cmd, track: false)
        }
        // Re-register any known push token (the relay's v1 store may have
        // dropped it across a restart; spec §5.5 / store.rs module docs), unless
        // this pairing is muted (in which case nothing is sent).
        await reconcilePushToken(on: ch)
    }

    /// Bring the relay's push-token state for this pairing in line with our
    /// intent (spec §5.5 / remote-control-b8d.10), sending at most one frame:
    ///  - unmuted + a token we haven't registered this session → `register_push_token`
    ///    (and record it so a repeat call doesn't double-register);
    ///  - muted while a token IS registered → `unregister_push_token` (and forget it);
    ///  - otherwise (muted with nothing registered, or already up to date) → no-op.
    /// Plain relay-plane frames (opaque token, outside E2E); a send failure is
    /// swallowed — it will be retried on the next `auth_ok`.
    private func reconcilePushToken(on ch: any WebSocketChannel) async {
        guard let record else { return }
        let pairingId = Wire.PairingId(record.pairingId)
        if pushMuted {
            if registeredToken != nil {
                try? await ch.send(.unregisterPushToken(pairingId: pairingId))
                registeredToken = nil
            }
        } else if let pushToken, registeredToken != pushToken.token {
            try? await ch.send(.registerPushToken(
                pairingId: pairingId,
                token: pushToken.token,
                environment: pushToken.environment
            ))
            registeredToken = pushToken.token
        }
    }

    private func handleEnvelope(_ env: Wire.EncryptedEnvelope, on ch: any WebSocketChannel) async {
        guard var record else {
            transportDiag.notice("FDDIAG recv envelope seq=\(env.seq) DROP: no pairing record")
            return
        }
        guard env.pairingId.rawValue == record.pairingId else {
            transportDiag.notice("FDDIAG recv envelope seq=\(env.seq) DROP: pairing mismatch env=\(env.pairingId.rawValue, privacy: .public) record=\(record.pairingId, privacy: .public)")
            return
        }
        transportDiag.notice("FDDIAG recv envelope seq=\(env.seq) sender=\(String(describing: env.sender), privacy: .public) cursor=\(record.lastReceivedSeq)")
        // The phone only consumes desktop→phone traffic.
        guard env.sender == .desktop else {
            transportDiag.notice("FDDIAG recv envelope seq=\(env.seq) DROP: sender not desktop")
            return
        }
        // Accept a strictly-newer seq (normal, §6.4 dedup), OR an explicit stream
        // reset: a seq of 1 while we already hold a higher cursor is the desktop
        // restarting its outbound stream after the relay lost its seq state
        // (remote-control-bbf). Steady state never re-emits seq 1 (it is
        // monotonic), so this can only be a genuine reset — accept it instead of
        // dropping it as a duplicate, or the recovered feed would stall forever.
        let isReset = env.seq == 1 && record.lastReceivedSeq >= 1
        guard env.seq > record.lastReceivedSeq || isReset else {
            transportDiag.notice("FDDIAG recv envelope seq=\(env.seq) DROP: dedup (cursor=\(record.lastReceivedSeq))")
            return
        }
        guard let e2e else {
            transportDiag.notice("FDDIAG recv envelope seq=\(env.seq) DROP: e2e channel is nil")
            return
        }

        // Open + decode BEFORE advancing the cursor: a failed open / AAD
        // mismatch must be rejected without acking or advancing (§7.1).
        let plaintext: Data
        do {
            plaintext = try e2e.open(
                seq: env.seq,
                sender: .desktop,
                sentAtMs: env.sentAtMs,
                nonceB64: env.nonce,
                ciphertextB64: env.ciphertext
            )
        } catch {
            transportDiag.notice("FDDIAG recv envelope seq=\(env.seq) DROP: OPEN FAILED: \(String(describing: error), privacy: .public)")
            return
        }
        let message: Wire.DesktopToPhone
        do {
            message = try JSONDecoder().decode(Wire.DesktopToPhone.self, from: plaintext)
        } catch {
            let preview = String(data: plaintext.prefix(280), encoding: .utf8) ?? "<non-utf8>"
            transportDiag.error("FDDIAG recv envelope seq=\(env.seq) DROP: DECODE FAILED: \(String(describing: error), privacy: .public) json=\(preview, privacy: .public)")
            return
        }
        transportDiag.notice("FDDIAG recv envelope seq=\(env.seq) OK decoded \(String(describing: message).prefix(40), privacy: .public) — acking cursor=\(env.seq)")

        // Commit the cursor durably, then publish and ack contiguous receipt. A
        // reset moves the cursor *backwards* to the new stream epoch, so it uses
        // the non-monotonic setter; the normal path stays monotonic.
        record.lastReceivedSeq = env.seq
        self.record = record
        if isReset {
            _ = try? recordStore.resetInboundCursor(to: env.seq, pairingId: record.pairingId)
        } else {
            _ = try? recordStore.setLastReceivedSeq(env.seq, pairingId: record.pairingId)
        }

        if case let .commandAck(ack) = message {
            resolvePending(ack)
        }
        emit(.message(message))

        try? await ch.send(.ack(
            pairingId: Wire.PairingId(record.pairingId),
            cursor: env.seq
        ))
    }

    // MARK: - Outbound

    private func sendEnvelope(for command: Wire.PhoneCommand, track: Bool) async {
        let commandId = command.commandId
        guard phase == .live, let ch = channel, let e2e, var record else {
            if track { fail(commandId, "not connected") }
            return
        }
        // Never send blind when the desktop is known-absent (§5.3).
        if peerConnected == false {
            if track { fail(commandId, "peer unavailable") }
            return
        }

        let plaintext: Data
        do {
            plaintext = try JSONEncoder().encode(command)
        } catch {
            if track { fail(commandId, "encode failed") }
            return
        }

        let next = record.lastSentSeq + 1
        let sentAt = now()
        let sealed: (nonceB64: String, ciphertextB64: String)
        do {
            sealed = try e2e.seal(plaintext, seq: next, sentAtMs: sentAt)
        } catch {
            if track { fail(commandId, "seal failed") }
            return
        }

        let envelope = Wire.EncryptedEnvelope(
            pairingId: Wire.PairingId(record.pairingId),
            seq: next,
            sender: .phone,
            sentAtMs: sentAt,
            nonce: sealed.nonceB64,
            ciphertext: sealed.ciphertextB64
        )
        do {
            try await ch.send(.envelope(envelope))
        } catch {
            if track { fail(commandId, "send failed") }
            return
        }

        // Commit the outbound cursor only after a successful send (§6.1).
        record.lastSentSeq = next
        self.record = record
        _ = try? recordStore.setLastSentSeq(next, pairingId: record.pairingId)

        if track {
            emit(.delivery(commandId: commandId, state: .sending))
            let timeout = Task { [weak self] in
                try? await Task.sleep(for: self?.config.commandTimeout ?? .seconds(10))
                if Task.isCancelled { return }
                await self?.timeoutCommand(commandId)
            }
            pending[commandId] = PendingCommand(seq: next, timeout: timeout)
        }
    }

    private func resolvePending(_ ack: Wire.CommandAck) {
        guard let p = pending.removeValue(forKey: ack.commandId) else { return }
        p.timeout.cancel()
        emit(.delivery(commandId: ack.commandId, state: .delivered(ack.outcome)))
    }

    private func timeoutCommand(_ commandId: Wire.CommandId) {
        guard pending.removeValue(forKey: commandId) != nil else { return }
        emit(.delivery(commandId: commandId, state: .failed(reason: "timed out")))
    }

    private func fail(_ commandId: Wire.CommandId, _ reason: String) {
        emit(.delivery(commandId: commandId, state: .failed(reason: reason)))
    }

    // MARK: - Helpers

    private func deriveChannel(_ record: PairingRecord) throws -> E2EChannel {
        guard let peerPub = Data(base64Encoded: record.peerKeyAgreementPublicKeyB64) else {
            throw E2EChannelError.invalidKeyMaterial
        }
        return try E2EChannel.derive(
            identityPrivateScalar: keyAgreement.privateScalar,
            peerPublicKeyX963: peerPub,
            pairingID: record.pairingId,
            salt: record.salt,
            role: .phone
        )
    }

    private func setLink(_ state: RemoteLinkState) {
        guard state != linkState else { return }
        linkState = state
        emit(.link(state))
    }

    private func emit(_ event: TransportEvent) {
        eventHandler?(event)
    }

    private func isFatal(_ code: Wire.RelayErrorCode) -> Bool {
        switch code {
        case .authFailed, .unsupportedVersion, .notAuthenticated, .badFrame, .internalError:
            return true
        case .unknownPairing, .pairingClaimRejected, .peerUnavailable, .rateLimited,
             .seqViolation, .unknown:
            // `seqViolation` is handled by re-syncing (see `handleSeqViolation`);
            // `unknown` is a forward-compat advisory we don't understand — never
            // tear the link down for either.
            return false
        }
    }

    /// The relay rejected one of our outbound command envelopes as non-monotonic:
    /// it lost its in-memory seq watermark (restart/redeploy) while we kept our
    /// persisted outbound cursor (remote-control-bbf). Rewind the cursor to 0 so
    /// the next command restarts at seq 1, which a fresh relay accepts. In-flight
    /// commands time out to "not delivered — retry"; a retry re-sends at the new
    /// low seq under the same command id (the desktop dedups idempotently).
    private func handleSeqViolation(_ pairingId: Wire.PairingId?) async {
        guard var record, pairingId == nil || pairingId?.rawValue == record.pairingId else { return }
        record.lastSentSeq = 0
        self.record = record
        _ = try? recordStore.resetOutboundCursor(pairingId: record.pairingId)
    }

    private func shortToken() -> String {
        UUID().uuidString.replacingOccurrences(of: "-", with: "").prefix(12).lowercased()
    }
}
