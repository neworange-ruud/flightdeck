//
//  NotificationPreferencesTests.swift
//  FlightDeckRemoteTests
//
//  The persisted notification-preferences model (PRD §5.6/§9.2): the three
//  independent toggles and per-project mute, each round-tripping through the
//  injected store.
//

import Testing
@testable import FlightDeckRemote

@MainActor
struct NotificationPreferencesTests {

    @Test func loadsDefaultsAllOn() {
        let prefs = NotificationPreferences(store: InMemoryNotificationSettingsStore())
        #expect(prefs.agentNeedsInput)
        #expect(prefs.agentFinished)
        #expect(prefs.completionChime)
        #expect(prefs.mutedProjectIds.isEmpty)
    }

    @Test func loadsPersistedValues() {
        let store = InMemoryNotificationSettingsStore(
            settings: NotificationSettings(
                agentNeedsInput: false,
                agentFinished: true,
                completionChime: false,
                mutedProjectIds: ["p1"]))
        let prefs = NotificationPreferences(store: store)
        #expect(prefs.agentNeedsInput == false)
        #expect(prefs.completionChime == false)
        #expect(prefs.isMuted(projectId: "p1"))
    }

    @Test func togglesPersistIndependently() {
        let store = InMemoryNotificationSettingsStore()
        let prefs = NotificationPreferences(store: store)

        prefs.agentFinished = false
        #expect(store.settings.agentFinished == false)
        // Others unaffected.
        #expect(store.settings.agentNeedsInput == true)
        #expect(store.settings.completionChime == true)

        prefs.completionChime = false
        #expect(store.settings.completionChime == false)
        #expect(store.settings.agentNeedsInput == true)
    }

    @Test func muteAndUnmutePersist() {
        let store = InMemoryNotificationSettingsStore()
        let prefs = NotificationPreferences(store: store)

        prefs.setMuted(true, projectId: "p1")
        #expect(prefs.isMuted(projectId: "p1"))
        #expect(store.settings.mutedProjectIds == ["p1"])

        prefs.setMuted(true, projectId: "p2")
        #expect(store.settings.mutedProjectIds == ["p1", "p2"])

        prefs.setMuted(false, projectId: "p1")
        #expect(prefs.isMuted(projectId: "p1") == false)
        #expect(store.settings.mutedProjectIds == ["p2"])
    }

    @Test func redundantMuteDoesNotRepersist() {
        let store = InMemoryNotificationSettingsStore()
        let prefs = NotificationPreferences(store: store)
        prefs.setMuted(true, projectId: "p1")
        let savesAfterFirst = store.saveCount
        // Muting an already-muted project is a no-op (no extra save).
        prefs.setMuted(true, projectId: "p1")
        #expect(store.saveCount == savesAfterFirst)
    }
}
