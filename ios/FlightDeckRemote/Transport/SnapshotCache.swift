//
//  SnapshotCache.swift
//  FlightDeckRemote
//
//  Persists the latest `Wire.StateSnapshot` + a capped per-session transcript
//  window to disk (JSON, Application Support, excluded from iCloud/device
//  backup) so the app can show a "last-known state" the instant it launches
//  — before the transport has even attempted to reconnect (PRD §9.2:
//  "when disconnected, show cached last-known transcript and status,
//  read-only and clearly marked stale").
//
//  Writes are coalesced with a short debounce (~2s) so a burst of live
//  updates (e.g. a `status_update` immediately followed by a `rollup`)
//  produces one disk write, not one per message. The debounce delay is
//  injected as a `SnapshotCacheClock` so tests can drive it deterministically
//  instead of waiting on a real clock — the same "swap the real thing for a
//  controllable double" shape `BiometricAuthenticating` already uses
//  elsewhere in this codebase.
//
//  `TransportStore` holds an optional `SnapshotCache` (`cache:` init param,
//  `nil` by default so every existing/unit-test construction of
//  `TransportStore(client:)` is unaffected) and calls `scheduleSave` from its
//  own fold step on meaningful updates — see that file's doc comment for the
//  additive hook. `TransportStoreFactory` owns reading the cache back with
//  `load()` and seeding it into the store via `TransportStore.seedFromCache`
//  before the transport connects.
//

import Foundation

/// Abstraction over the debounce delay so tests can control timing without
/// waiting on a real clock.
protocol SnapshotCacheClock: Sendable {
    func sleep(for duration: Duration) async
}

/// Real, `Task.sleep`-backed clock used in production.
struct RealSnapshotCacheClock: SnapshotCacheClock {
    func sleep(for duration: Duration) async {
        try? await Task.sleep(for: duration)
    }
}

/// The full persisted payload: the last snapshot plus a capped transcript
/// window per session. A dedicated DTO (rather than a raw
/// `[Wire.SessionId: [Wire.TranscriptItem]]` dictionary) sidesteps
/// `JSONEncoder`'s awkward encoding of dictionaries with non-string keys.
/// Top-level (not nested in the `@MainActor` class) so its synthesized
/// `Codable` conformance stays nonisolated; `SnapshotCache.CachedState`
/// remains the spelling call sites use via the typealias below.
struct SnapshotCacheState: Codable, Equatable, Sendable {
    var snapshot: Wire.StateSnapshot
    var transcripts: [TranscriptEntry]
    /// Wall-clock time (unix ms) this state was written, for diagnostics.
    var cachedAtMs: Int64

    struct TranscriptEntry: Codable, Equatable, Sendable {
        var sessionId: Wire.SessionId
        var items: [Wire.TranscriptItem]
    }

    init(snapshot: Wire.StateSnapshot, transcripts: [TranscriptEntry], cachedAtMs: Int64) {
        self.snapshot = snapshot
        self.transcripts = transcripts
        self.cachedAtMs = cachedAtMs
    }
}

@MainActor
final class SnapshotCache {

    typealias CachedState = SnapshotCacheState

    /// Cap on how many trailing transcript items are retained per session.
    static let transcriptCapPerSession = 100

    private let fileURL: URL
    private let debounceInterval: Duration
    private let clock: any SnapshotCacheClock
    private let now: @Sendable () -> Int64

    /// The state most recently requested to be saved — always the latest,
    /// regardless of which in-flight debounce task ends up performing the
    /// write (see `scheduleSave`).
    private var pendingState: CachedState?
    /// Monotonic counter identifying the most recent `scheduleSave` call; a
    /// debounce task only writes if it's still the newest one once its delay
    /// elapses, so rapid-fire updates collapse into a single write carrying
    /// the latest data.
    private var generation = 0

    init(
        fileURL: URL,
        debounceInterval: Duration = .seconds(2),
        clock: any SnapshotCacheClock = RealSnapshotCacheClock(),
        now: @escaping @Sendable () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }
    ) {
        self.fileURL = fileURL
        self.debounceInterval = debounceInterval
        self.clock = clock
        self.now = now
    }

    /// The default on-disk location: `Application Support/FlightDeckRemote/snapshot-cache.json`,
    /// created on first use and excluded from iCloud/device backup — this is
    /// disposable last-known-state, not user data worth backing up (and
    /// backing up transcript content would be a privacy surprise).
    static func defaultFileURL(fileManager: FileManager = .default) -> URL {
        let base = (try? fileManager.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true
        )) ?? fileManager.temporaryDirectory
        let dir = base.appendingPathComponent("FlightDeckRemote", isDirectory: true)
        if !fileManager.fileExists(atPath: dir.path) {
            try? fileManager.createDirectory(at: dir, withIntermediateDirectories: true)
        }
        var url = dir.appendingPathComponent("snapshot-cache.json")
        var resourceValues = URLResourceValues()
        resourceValues.isExcludedFromBackup = true
        try? url.setResourceValues(resourceValues)
        return url
    }

    // MARK: - Read

    /// Loads the persisted state, or `nil` if there is none / it fails to
    /// decode. A corrupt cache file is treated the same as "no cache" — this
    /// is disposable last-known-state, never a source of truth.
    func load() -> CachedState? {
        guard let data = try? Data(contentsOf: fileURL) else { return nil }
        return try? JSONDecoder().decode(CachedState.self, from: data)
    }

    // MARK: - Write

    /// Requests a debounced save of `snapshot` + `transcripts` (each session's
    /// transcript capped to the last `transcriptCapPerSession` items).
    /// Coalesces with any save already in flight: only the last request
    /// within the debounce window actually reaches disk, and it always
    /// carries the latest data (`pendingState` is overwritten synchronously
    /// on every call, independent of which debounce task fires the write).
    func scheduleSave(snapshot: Wire.StateSnapshot, transcripts: [Wire.SessionId: [Wire.TranscriptItem]]) {
        let entries = transcripts
            .map { sessionId, items in
                CachedState.TranscriptEntry(sessionId: sessionId, items: Array(items.suffix(Self.transcriptCapPerSession)))
            }
            .sorted { $0.sessionId.rawValue < $1.sessionId.rawValue } // stable ordering for round-trip equality
        pendingState = CachedState(snapshot: snapshot, transcripts: entries, cachedAtMs: now())

        generation += 1
        let thisGeneration = generation
        let clock = self.clock
        let interval = debounceInterval
        Task { @MainActor [weak self] in
            await clock.sleep(for: interval)
            self?.flushIfCurrent(generation: thisGeneration)
        }
    }

    private func flushIfCurrent(generation requested: Int) {
        guard requested == generation, let pendingState else { return }
        write(pendingState)
    }

    private func write(_ state: CachedState) {
        guard let data = try? JSONEncoder().encode(state) else { return }
        try? data.write(to: fileURL, options: .atomic)
    }
}
