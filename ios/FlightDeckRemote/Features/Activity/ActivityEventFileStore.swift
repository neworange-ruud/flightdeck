//
//  ActivityEventFileStore.swift
//  FlightDeckRemote
//
//  Disk-backed `ActivityEventPersisting` (Navigation/ActivityStore.swift):
//  one JSON file in Application Support, so the Activity feed survives app
//  restarts (PRD §5.7). Mirrors `SnapshotCache`'s storage shape
//  (Transport/SnapshotCache.swift) but kept as its own small file — the
//  Activity feed owns this persistence independently of the transport/
//  offline-cache concern, and writes are lightweight/un-debounced (a feed
//  event is rare compared to a live status stream).
//

import Foundation

struct ActivityEventFileStore: ActivityEventPersisting {
    private let fileURL: URL

    init(fileURL: URL = ActivityEventFileStore.defaultFileURL()) {
        self.fileURL = fileURL
    }

    static func defaultFileURL(fileManager: FileManager = .default) -> URL {
        let base = (try? fileManager.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true
        )) ?? fileManager.temporaryDirectory
        let dir = base.appendingPathComponent("FlightDeckRemote", isDirectory: true)
        if !fileManager.fileExists(atPath: dir.path) {
            try? fileManager.createDirectory(at: dir, withIntermediateDirectories: true)
        }
        return dir.appendingPathComponent("activity-events.json")
    }

    func load() -> ActivityPersistedState {
        guard let data = try? Data(contentsOf: fileURL),
              let state = try? JSONDecoder().decode(ActivityPersistedState.self, from: data)
        else { return ActivityPersistedState() }
        return state
    }

    func save(_ state: ActivityPersistedState) {
        guard let data = try? JSONEncoder().encode(state) else { return }
        try? data.write(to: fileURL, options: .atomic)
    }
}
