//
//  FeedUnreadFileStore.swift
//  FlightDeckRemote
//
//  Disk-backed `FeedUnreadPersisting` (Navigation/FeedUnreadStore.swift): one
//  JSON file in Application Support so the unified Feed's per-item unread
//  watermarks survive app restarts (remote-control-fa8). Reuses the exact
//  storage shape the (removed) Activity feed's `ActivityEventFileStore` used —
//  writes are lightweight/un-debounced (an event or a row-open is rare compared
//  to a live status stream).
//

import Foundation

struct FeedUnreadFileStore: FeedUnreadPersisting {
    private let fileURL: URL

    init(fileURL: URL = FeedUnreadFileStore.defaultFileURL()) {
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
        return dir.appendingPathComponent("feed-unread.json")
    }

    func load() -> FeedUnreadPersistedState {
        guard let data = try? Data(contentsOf: fileURL),
              let state = try? JSONDecoder().decode(FeedUnreadPersistedState.self, from: data)
        else { return FeedUnreadPersistedState() }
        return state
    }

    func save(_ state: FeedUnreadPersistedState) {
        guard let data = try? JSONEncoder().encode(state) else { return }
        try? data.write(to: fileURL, options: .atomic)
    }
}
