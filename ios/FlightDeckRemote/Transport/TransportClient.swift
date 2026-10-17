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
//  Relay vs direct (v4, remote-control-gz0d): the same state machine drives
//  both the relay and the Mac's direct link over Tailscale (§5.10), because
//  the Mac speaks the relay's frame set in the relay's role. Each session
//  resolves a `TransportLink` from the pairing record and `TransportPlane`
//  (DirectConnection.swift): it picks the URL, the auth signature (bare nonce
//  vs domain-separated) and which frames may ride the link. Everything else —
//  E2E, seq, ack, resume, dedup, recovery, backoff — is one code path.
//
//  Push-only relays (v4, §5.9): a relay announcing `relay_mode: push_only` (or
//  refusing a data frame with `relay_mode_push_only`) carries no envelopes.
//  On a data-plane relay link the session then stays up for push-token
//  registration/revoke, but sends no resume/envelope/ack, reports the data
//  plane as disconnected, and fails commands honestly — no reconnect loop.
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
        /// How long a session must survive past `auth_ok` before it counts as
        /// genuinely good and resets the reconnect backoff (remote-control-arg).
        ///
        /// Reaching `auth_ok` is far too weak a signal on its own: a session that
        /// authenticates and then immediately dies — a relay that accepts the
        /// hello and drops the leg, a supersede race, a peer rejecting every
        /// envelope — resets the schedule to its 1s floor on every attempt and
        /// hammers a live relay indefinitely. Requiring the session to *last*
        /// means a connection that cannot make progress escalates 1s → 2 → 4 →
        /// … → 60s like any other failure.
        var minimumGoodSession: Duration = .seconds(30)

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
    /// Which half of the pairing's traffic this client carries (data plane, or
    /// relay control for a direct-mode pairing). The concrete link is resolved
    /// per session from the record — see `TransportLink.resolve`.
    private let plane: TransportPlane
    private let connector: any WebSocketConnecting
    private let clientInfo: Wire.ClientInfo
    /// Source of the OPTIONAL shared relay password (remote-control-uq7),
    /// evaluated on EVERY connect so a password captured/changed at pairing is
    /// picked up without rebuilding the client. Returns `nil` for a
    /// local/dev relay with no password, in which case the `hello` omits the
    /// `relay_password` key entirely. Never logged.
    private let relayPassword: @Sendable () -> String?
    private let config: Config
    private let jitter: @Sendable () -> Double
    private let now: @Sendable () -> Int64

    // MARK: - Runtime state

    private var eventHandler: (@Sendable (TransportEvent) -> Void)?
    private var supervisor: Task<Void, Never>?
    /// The in-progress inter-attempt backoff sleep, held so `reconnectNow()`
    /// (remote-control-0ef.21 "Retry now") can wake it early. `nil` whenever the
    /// supervisor isn't currently waiting between attempts.
    private var backoffTask: Task<Void, Never>?
    /// Set by `reconnectNow()`: the next supervisor iteration resets the backoff
    /// attempt counter and reconnects immediately instead of waiting.
    private var forcedReconnect = false

    private var record: PairingRecord?
    private var channel: (any WebSocketChannel)?
    private var e2e: E2EChannel?
    private var pinger: Task<Void, Never>?

    private var linkState: RemoteLinkState = .disconnected
    private var latencyMs: Int = 0
    private var phase: Phase = .idle
    private var sessionAuthed = false
    private var peerConnected: Bool?

    /// The link the current (or last) session drives, resolved from the record
    /// at session start. `.relay` until the first session resolves it.
    private var link: TransportLink = .relay
    /// Set when the relay this data-plane session talks to carries no
    /// envelopes: it announced `relay_mode: push_only` in `hello_ok`, or
    /// refused a data frame with `relay_mode_push_only` (§5.9). Per session — a
    /// relay may change mode across restarts.
    private var relayRefusesDataPlane = false

    /// Whether this session may carry envelopes / resume / acks right now.
    private var dataPlaneUsable: Bool {
        link.carriesDataPlane && !relayRefusesDataPlane
    }

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
        plane: TransportPlane = .data,
        connector: any WebSocketConnecting,
        clientInfo: Wire.ClientInfo? = nil,
        relayPassword: @escaping @Sendable () -> String? = { RelayPasswordStore().load() },
        config: Config = Config(),
        jitter: @escaping @Sendable () -> Double = { Double.random(in: 0..<1) },
        now: @escaping @Sendable () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }
    ) {
        self.identity = identity
        self.keyAgreement = keyAgreement
        self.recordStore = recordStore
        self.targetPairingId = pairingId
        self.plane = plane
        self.connector = connector
        self.clientInfo = clientInfo ?? Wire.ClientInfo(
            appVersion: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "1.0",
            platform: "ios",
            osVersion: nil
        )
        self.relayPassword = relayPassword
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

    /// Force an immediate reconnect (remote-control-0ef.21 "Retry now"): reset
    /// the backoff schedule and drop straight into a fresh connect, whether the
    /// client is mid-backoff or sitting on a silently-dead socket. Closing the
    /// channel breaks a stuck `runSession` receive loop; cancelling `backoffTask`
    /// wakes an in-progress backoff wait. No-op if the supervisor isn't running
    /// (nothing to reconnect — e.g. no pairing record, or backgrounded/stopped).
    func reconnectNow() async {
        guard supervisor != nil else { return }
        forcedReconnect = true
        backoffTask?.cancel()
        backoffTask = nil
        await channel?.close()
    }

    /// Stop the supervisor and tear down the current connection.
    func stop() async {
        supervisor?.cancel()
        supervisor = nil
        backoffTask?.cancel()
        backoffTask = nil
        forcedReconnect = false
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

    /// The current link state (test/inspection convenience). For a
    /// `.relayControl` client this is tracked but never emitted.
    func currentLinkState() -> RemoteLinkState { linkState }

    /// The link the current (or last) session resolved (test/inspection).
    func currentLink() -> TransportLink { link }

    /// Whether the relay this data-plane session reached refuses envelopes
    /// (push-only, §5.9) — test/inspection convenience.
    func relayIsPushOnly() -> Bool { relayRefusesDataPlane }

    /// Which plane this client carries.
    nonisolated var transportPlane: TransportPlane { plane }

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

    /// Send a phone→relay `revoke` for this pairing (spec §5.8,
    /// remote-control-b8d.11 unpair-one). Best-effort and fire-and-forget: a
    /// plaintext relay-plane frame (NOT a sealed command), put on the wire only
    /// if this pairing's session is currently live. If the relay is unreachable
    /// (no live session) or the send fails, this is a no-op and returns `false`
    /// — the caller still removes the pairing locally; the relay's revoke is
    /// idempotent, so a retry (or the desktop noticing the membership is gone)
    /// reconciles later. Deliberately does not check `peerConnected`: the relay
    /// processes a revoke and notifies the desktop on its next connect
    /// regardless of whether the desktop is online right now. Returns whether a
    /// frame was actually sent (diagnostic only).
    ///
    /// Works on every link: on the relay (relay mode, or the `.relayControl`
    /// client of a direct pairing) it is the authoritative revoke; on the
    /// direct link it is the courtesy copy §5.10 allows, so a connected Mac
    /// drops the pairing immediately.
    @discardableResult
    func revokePairing() async -> Bool {
        guard phase == .live, let ch = channel, let record else { return false }
        do {
            try await ch.send(.revoke(pairingId: Wire.PairingId(record.pairingId)))
            return true
        } catch {
            return false
        }
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
            let startedAt = now()
            let authed = await runSession()
            if Task.isCancelled { break }
            setLink(.disconnected)
            // A user-requested "Retry now" (remote-control-0ef.21) resets the
            // backoff and reconnects immediately, skipping the wait entirely.
            if forcedReconnect {
                forcedReconnect = false
                attempt = 0
                continue
            }
            // Only a session that authenticated AND lasted counts as good enough
            // to clear the backoff. `auth_ok` alone is not progress: a session
            // that authenticates and dies instantly would otherwise pin the
            // schedule at its 1s floor and reconnect against a live relay
            // forever (remote-control-arg). Escalate on anything shorter.
            let lastedMs = max(0, now() - startedAt)
            let goodSession = authed
                && lastedMs >= config.minimumGoodSession.milliseconds
            attempt = goodSession ? 0 : attempt + 1
            let delay = Backoff.delay(attempt: attempt, jitterUnit: jitter())
            await backoffSleep(delay)
            // "Retry now" during the wait woke it early — reset the schedule so
            // the immediate attempt starts from a 1s floor again.
            if forcedReconnect {
                forcedReconnect = false
                attempt = 0
            }
        }
    }

    /// Wait `delay` between reconnect attempts, staying responsive to BOTH
    /// supervisor cancellation (`stop()`) and an explicit `reconnectNow()`
    /// (remote-control-0ef.21). The latter cancels `backoffTask` to wake the
    /// wait immediately; the cancellation handler forwards a `stop()` cancel to
    /// the same sleeper so teardown never blocks on a full backoff interval.
    private func backoffSleep(_ delay: Duration) async {
        let sleeper = Task<Void, Never> { _ = try? await Task.sleep(for: delay) }
        backoffTask = sleeper
        await withTaskCancellationHandler {
            await sleeper.value
        } onCancel: {
            sleeper.cancel()
        }
        backoffTask = nil
    }

    /// One connection session. Returns whether it reached `auth_ok` (so the
    /// supervisor can reset backoff after a genuinely-good session dropped).
    private func runSession() async -> Bool {
        guard let record else { return false }
        // Relay vs direct is decided here, from the record, every session.
        link = TransportLink.resolve(plane: plane, record: record)
        relayRefusesDataPlane = false
        guard let url = link.url(for: record) else { return false }

        let ch: any WebSocketChannel
        do {
            ch = try await connector.connect(to: url)
        } catch {
            return false
        }
        channel = ch
        sessionAuthed = false
        phase = .awaitingHelloOk
        // The peer's presence is per-SESSION knowledge: a fresh socket knows
        // nothing about the desktop until the relay says so. Carrying the last
        // session's belief over broke refresh-on-reconnect in both directions
        // (remote-control-e9l):
        //  - a stale `false` made the post-`auth_ok` `request_snapshot` fail
        //    fast as "peer unavailable", so the desktop was never asked;
        //  - a stale `true` made the compensating re-issue below
        //    (`connected, wasConnected != true`) look like a no-op change and
        //    skip, because it thought the desktop had never left.
        // `nil` (unknown) lets the send through and makes the first real
        // `peer_presence` frame a genuine transition.
        peerConnected = nil

        // Derive the E2E channel for this pairing up front (needed the instant a
        // replayed envelope arrives after resume).
        do {
            e2e = try deriveChannel(record)
        } catch {
            await ch.close()
            channel = nil
            return false
        }

        // hello. Present the shared relay password (remote-control-uq7) if one
        // is stored — re-read every connect so a change is picked up; nil omits
        // the `relay_password` key so an unconfigured relay is unaffected. Never
        // sent to the Mac on the direct link (it ignores it, §5.10, and it is a
        // relay secret).
        do {
            try await ch.send(.hello(
                protocolVersion: Wire.protocolVersion,
                role: .phone,
                deviceId: Wire.DeviceId(identity.deviceId),
                client: clientInfo,
                relayPassword: link.isRelay ? relayPassword() : nil
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
        case let .helloOk(_, _, _, relayMode):
            if phase == .awaitingHelloOk { phase = .awaitingChallenge }
            // A push-only relay carries no envelopes (§5.9). Only meaningful
            // for a relay-mode pairing's data plane: the control-plane client
            // of a direct pairing never sends data frames anyway, and the Mac
            // omits the field on the direct link.
            if relayMode == .pushOnly, link == .relay {
                markRelayRefusesDataPlane()
            }
            return true

        case .versionIncompatible:
            return false

        case let .authChallenge(nonce, _):
            guard phase == .awaitingChallenge else { return true }
            let signature: String
            do {
                // Bare nonce for the relay (§5.1); domain-separated
                // `"flightdeck-remote-direct-v1:" ‖ nonce` for the Mac (§5.10).
                signature = try link.signChallenge(nonce, with: identity)
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
            // A push-only relay authenticates fine but carries no data: keep
            // the data plane reported as down so the UI stays honest.
            if dataPlaneUsable { setLink(.connected(latencyMs: 0)) }
            transportDiag.notice("FDDIAG connection LIVE (auth_ok) pairing=\(self.record?.pairingId ?? "nil", privacy: .public) link=\(String(describing: self.link), privacy: .public)")
            await onAuthenticated(ch)
            return true

        case let .peerPresence(_, peer, state, _):
            // Presence on a link that carries no data for this pairing (the
            // control-plane relay of a direct pairing, or a push-only relay)
            // says nothing about whether a command could be delivered — the
            // data plane's own link owns that. Ignore it.
            guard dataPlaneUsable else { return true }
            let connected = state == .connected
            if peer == .desktop {
                let wasConnected = peerConnected
                peerConnected = connected
                // The desktop just came back (offline/unknown → online). The
                // post-`auth_ok` resume+snapshot may have been suppressed while
                // it was absent (`sendEnvelope` fails fast when
                // `peerConnected == false`), so the UI would otherwise stay
                // stale on deltas alone (dropped when `snapshot == nil` —
                // TransportStore). Re-issue resume + request_snapshot now that
                // the desktop is reachable (remote-control-0ef.19).
                if connected, wasConnected != true, phase == .live, let ch = channel {
                    await requestResumeAndSnapshot(on: ch)
                }
            }
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
            // Only a data-plane link consumes (and acks) envelopes; a control-
            // plane relay link must never advance this pairing's cursors.
            guard dataPlaneUsable else { return true }
            await handleEnvelope(env, on: ch)
            return true

        case let .pong(clientTimeMs, _):
            latencyMs = max(0, Int(now() - clientTimeMs))
            if dataPlaneUsable { setLink(.connected(latencyMs: latencyMs)) }
            return true

        case let .error(code, _, _, _) where code == .relayModePushOnly:
            // A push-only relay refused a data frame (§5.9). Advisory: the
            // connection stays useful for push tokens and revoke, so keep it,
            // but stop all data-plane traffic on it. On any other link this is
            // unexpected and simply ignored.
            if link == .relay { markRelayRefusesDataPlane() }
            return true

        case let .error(code, _, pairingId, expectedSeq):
            // `seq_violation` is recoverable, not fatal — but it covers two
            // opposite faults, told apart by `expectedSeq`:
            //   present -> OUR OUTBOUND seq ran ahead; realign to what the relay
            //              named and re-request a snapshot (remote-control-zv3).
            //   absent  -> our INBOUND cursor is stale; drop it and re-sync (bbf).
            // Conflating them is what left a runaway sender with no way back:
            // only the inbound half was implemented, so the outbound stream
            // stayed wedged and every command died silently.
            // Cursor recovery is a data-plane concern; a control-plane link
            // must never touch the cursors the data-plane client owns.
            if code == .seqViolation, dataPlaneUsable {
                if let next = expectedSeq {
                    await handleSeqRealign(pairingId, nextSeq: next, on: ch)
                } else {
                    await handleSeqViolation(pairingId, on: ch)
                }
            }
            return !isFatal(code)

        case .bye:
            return false

        // The relay says this pairing is gone. The phone removes its own pairing
        // state eagerly when IT revokes (best-effort, idempotent —
        // remote-control-b8d.11), so this is normally just a confirmation of
        // something already done. But a revoke can also come from the DESKTOP
        // retiring a superseded pairing to this same phone (remote-control-4wk),
        // and there the phone must act: otherwise it keeps a record for a pairing
        // the relay no longer knows and reconnects it forever without ever
        // reaching `auth_ok`. Surfacing it is safe in both cases — dropping an
        // already-dropped pairing is a no-op. End the session: this client has
        // nothing left to do.
        case let .pairingRevoked(pairingId):
            guard record?.pairingId == pairingId.rawValue else { return true }
            emit(.pairingRevoked)
            return false

        // Frames the phone never receives in steady state, or handshake
        // restatements: ignore and keep the session alive. `revoke` is
        // phone→relay only.
        case .ack, .resume, .ping, .hello, .authResponse,
             .pairingOffer, .pairingOfferOk, .pairingClaim, .pairingClaimed,
             .registerPushToken, .unregisterPushToken, .pushTokenAck,
             .revoke, .pushNotify:
            return true
        }
    }

    /// The relay this data-plane session reached is push-only (§5.9): stop all
    /// data-plane traffic on it without ending the session (which would only
    /// reconnect into the same refusal), fail in-flight commands honestly, and
    /// report the data plane as down.
    private func markRelayRefusesDataPlane() {
        guard !relayRefusesDataPlane else { return }
        relayRefusesDataPlane = true
        transportDiag.notice("FDDIAG relay is push_only — no data plane for relay-mode pairing=\(self.record?.pairingId ?? "nil", privacy: .public)")
        for (commandId, p) in pending {
            p.timeout.cancel()
            fail(commandId, Self.pushOnlyReason)
        }
        pending.removeAll()
        setLink(.disconnected)
        emit(.relayPushOnly)
    }

    /// Delivery-honesty reason for a command refused because the relay is
    /// push-only.
    static let pushOnlyReason = "relay is push-only — re-pair over Tailscale"

    /// After `auth_ok`: resume from the persisted inbound cursor, request a
    /// fresh snapshot so the UI has current state immediately, then re-register
    /// any known push token.
    private func onAuthenticated(_ ch: any WebSocketChannel) async {
        await requestResumeAndSnapshot(on: ch)
        // Re-register any known push token (the relay's v1 store may have
        // dropped it across a restart; spec §5.5 / store.rs module docs), unless
        // this pairing is muted (in which case nothing is sent).
        await reconcilePushToken(on: ch)
    }

    /// Resume from the persisted inbound cursor and (config permitting) request
    /// a fresh snapshot. Sent after `auth_ok` (`onAuthenticated`) AND again
    /// whenever the desktop peer transitions back to present
    /// (remote-control-0ef.19): the initial post-auth snapshot request is
    /// dropped when the desktop is offline (`sendEnvelope` refuses while
    /// `peerConnected == false`), so re-issuing it on the peer's return is what
    /// refreshes an otherwise-stale UI.
    private func requestResumeAndSnapshot(on ch: any WebSocketChannel) async {
        // Never on a link without a data plane: the control-plane relay of a
        // direct pairing (§5.10) or a push-only relay (§5.9, which would answer
        // `relay_mode_push_only`).
        guard dataPlaneUsable, let record else { return }
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
        // Push tokens live on the relay only — the Mac answers
        // `register_push_token` with a fatal `bad_frame` (§5.10). A direct
        // pairing registers through its `.relayControl` client instead.
        guard link.carriesRelayControl, let record else { return }
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
        // Never put an envelope on a link that cannot carry it (§5.9/§5.10).
        guard dataPlaneUsable else {
            if track { fail(commandId, relayRefusesDataPlane ? Self.pushOnlyReason : "not connected") }
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
        // A control-plane client shares its pairing's store with the data-
        // plane client, so it may only surface what is genuinely the relay's
        // to say — never link state, presence, messages or delivery, which
        // belong to the data plane's honest connection UI.
        if plane == .relayControl {
            switch event {
            case .pairingRevoked, .machineName: break
            case .link, .message, .delivery, .presence, .relayPushOnly: return
            }
        }
        eventHandler?(event)
    }

    private func isFatal(_ code: Wire.RelayErrorCode) -> Bool {
        switch code {
        case .authFailed, .unsupportedVersion, .notAuthenticated, .badFrame, .internalError:
            return true
        case .unknownPairing, .pairingClaimRejected, .peerUnavailable, .rateLimited,
             .seqViolation, .relayModePushOnly, .unknown:
            // `seqViolation` is handled by re-syncing (see `handleSeqViolation`);
            // `unknown` is a forward-compat advisory we don't understand — never
            // tear the link down for either.
            return false
        }
    }

    /// The relay sent a `seq_violation` advisory for this pairing. It means our
    /// **inbound** cursor is stale — the desktop restarted its outbound stream,
    /// or the relay shed queued envelopes we still needed — so the seqs now
    /// arriving sit below `lastReceivedSeq` and our dedup would discard them.
    /// Drop the cursor, re-`resume` from scratch to pull whatever the relay still
    /// holds, and request a fresh snapshot.
    ///
    /// It does **not** mean "rewind your outbound cursor", and this used to do
    /// exactly that (remote-control-bbf, written for a relay that came back from
    /// a restart with an empty in-memory watermark). Against a relay that
    /// *persists* its watermark, that rewind is what produced the reported P0
    /// (remote-control-arg): the relay expected seq 60, we restarted at 1, it
    /// rejected us, we rewound to 0 and restarted at 1 again — at reconnect
    /// speed, forever. Inbound kept working the whole time, so it presented as a
    /// connectivity flap rather than as every outbound command dying.
    ///
    /// Worse, the advisory also arrives for a resume-resync, which is purely an
    /// inbound concern — so a phone that had been offline long enough for the
    /// desktop→phone queue to overflow would destroy a perfectly good outbound
    /// cursor on its next reconnect. That is remote-control-h1y's "cursors lost
    /// on an in-place app update": nothing was lost in the Keychain; the cursor
    /// was zeroed here, by us. The relay now adopts an unknown stream's starting
    /// seq and absorbs a genuine rewind itself, so `lastSentSeq` is left alone.
    /// The relay rejected our outbound envelopes because our `seq` ran ahead of
    /// its watermark, and named the seq it will accept next. Realign
    /// `lastSentSeq` so the next envelope is exactly `nextSeq`, then re-resume
    /// and ask for a fresh snapshot — everything we sent while ahead was dropped
    /// by the relay and never reached the desktop.
    ///
    /// Deliberately does NOT touch `lastReceivedSeq`: this fault is entirely
    /// about our outbound numbering, and zeroing a healthy inbound cursor here is
    /// the remote-control-h1y mistake in the opposite direction.
    private func handleSeqRealign(
        _ pairingId: Wire.PairingId?, nextSeq: UInt64, on ch: any WebSocketChannel
    ) async {
        guard var record, pairingId == nil || pairingId?.rawValue == record.pairingId else { return }
        // `nextSeq` is the relay's high_water + 1, so the next `lastSentSeq + 1`
        // must land on it.
        let realigned = nextSeq > 0 ? nextSeq - 1 : 0
        transportDiag.notice(
            "FDDIAG seq realign: lastSentSeq \(record.lastSentSeq, privacy: .public) -> \(realigned, privacy: .public) (relay expects \(nextSeq, privacy: .public))"
        )
        record.lastSentSeq = realigned
        self.record = record
        _ = try? recordStore.realignOutboundCursor(to: realigned, pairingId: record.pairingId)
        await requestResumeAndSnapshot(on: ch)
    }

    private func handleSeqViolation(_ pairingId: Wire.PairingId?, on ch: any WebSocketChannel) async {
        guard var record, pairingId == nil || pairingId?.rawValue == record.pairingId else { return }
        record.lastReceivedSeq = 0
        self.record = record
        _ = try? recordStore.resetInboundCursor(to: 0, pairingId: record.pairingId)
        await requestResumeAndSnapshot(on: ch)
    }

    private func shortToken() -> String {
        UUID().uuidString.replacingOccurrences(of: "-", with: "").prefix(12).lowercased()
    }
}
