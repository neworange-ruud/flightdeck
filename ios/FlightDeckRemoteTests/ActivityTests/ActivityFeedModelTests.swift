//
//  ActivityFeedModelTests.swift
//  FlightDeckRemoteTests
//
//  Covers `ActivityFeedModel` (PRD §5.7): project-name resolution for the
//  cell tag (with an honest raw-id fallback for an unknown project), and
//  tap-to-navigate reusing `AppRouter.pendingDeepLink` +
//  `ProjectsDeepLinkTranslator` — including the dead-session path, which
//  must surface a note and never navigate.
//

import Testing
import Foundation
@testable import FlightDeckRemote

/// In-memory `ActivityEventPersisting` double, mirrors `ActivityStoreTests`.
private final class FakeActivityEventPersisting: ActivityEventPersisting {
    var state: ActivityPersistedState
    init(state: ActivityPersistedState = ActivityPersistedState()) { self.state = state }
    func load() -> ActivityPersistedState { state }
    func save(_ state: ActivityPersistedState) { self.state = state }
}

@MainActor
@Suite struct ActivityFeedModelTests {

    private func makeTransportStore() throws -> TransportStore {
        let keychain = InMemoryKeychainStore()
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        let keyAgreement = try KeyAgreementKeys.loadOrCreate(store: keychain)
        let recordStore = PairingRecordStore(store: keychain)
        let client = TransportClient(
            identity: identity, keyAgreement: keyAgreement, recordStore: recordStore,
            connector: ScriptedConnector(channel: ScriptedChannel()))
        return TransportStore(client: client)
    }

    private func snapshot() -> Wire.StateSnapshot {
        let session = Wire.SessionState(
            sessionId: Wire.SessionId("s1"), projectId: Wire.ProjectId("p1"),
            name: "fix-login", agentType: .claudeCode, status: .idle,
            git: Wire.GitIndicators(branch: "main", added: 0, modified: 0, removed: 0,
                                     ahead: 0, behind: 0, drift: 0, hasUpstream: true),
            runningTimeSecs: 0, pendingQuestion: nil)
        let project = Wire.ProjectState(
            projectId: Wire.ProjectId("p1"), name: "flightdeck",
            rollup: Wire.StatusRollup(dot: .idle, summary: "idle", working: 0, idle: 1,
                                       needsInput: 0, manual: 0, agentCount: 1),
            sessions: [session])
        return Wire.StateSnapshot(serverTimeMs: 1, projects: [project])
    }

    private func event(projectId: String, sessionId: String) -> Wire.AgentEvent {
        Wire.AgentEvent(
            eventId: Wire.EventId("evt1"),
            kind: .finished(summary: "done", filesChanged: 0, readyToPush: false),
            deepLink: Wire.DeepLink(projectId: Wire.ProjectId(projectId), sessionId: Wire.SessionId(sessionId), itemId: nil),
            occurredAtMs: 1,
            title: "finished")
    }

    @Test func cellViewModelsResolveTheProjectNameFromTheSnapshot() throws {
        let transportStore = try makeTransportStore()
        transportStore.debugSeed(snapshot: snapshot())
        let activityStore = ActivityStore(persistence: FakeActivityEventPersisting())
        activityStore.ingest([event(projectId: "p1", sessionId: "s1")], tabSelected: false)
        let router = AppRouter(pairingStore: PairingStore())
        let model = ActivityFeedModel(activityStore: activityStore, transportStore: transportStore, router: router)

        let cells = model.cellViewModels(nowMs: 1)
        #expect(cells.first?.projectTag.hasPrefix("flightdeck ·") == true)
    }

    @Test func cellViewModelsFallBackToTheRawProjectIdWhenUnknown() throws {
        let transportStore = try makeTransportStore() // no snapshot at all
        let activityStore = ActivityStore(persistence: FakeActivityEventPersisting())
        activityStore.ingest([event(projectId: "ghost-project", sessionId: "s1")], tabSelected: false)
        let router = AppRouter(pairingStore: PairingStore())
        let model = ActivityFeedModel(activityStore: activityStore, transportStore: transportStore, router: router)

        let cells = model.cellViewModels(nowMs: 1)
        #expect(cells.first?.projectTag.hasPrefix("ghost-project ·") == true)
    }

    @Test func tappingAKnownEventNavigatesViaTheSharedDeepLinkPath() throws {
        let transportStore = try makeTransportStore()
        transportStore.debugSeed(snapshot: snapshot())
        let activityStore = ActivityStore(persistence: FakeActivityEventPersisting())
        activityStore.ingest([event(projectId: "p1", sessionId: "s1")], tabSelected: false)
        let router = AppRouter(pairingStore: PairingStore())
        router.selectedTab = .activity
        let model = ActivityFeedModel(activityStore: activityStore, transportStore: transportStore, router: router)

        model.handleTap(eventId: Wire.EventId("evt1"))

        #expect(router.selectedTab == .projects)
        #expect(router.pendingDeepLink == DeepLink(projectId: "p1", sessionId: "s1"))
        #expect(model.deadSessionNote == nil)
    }

    @Test func tappingADeadSessionShowsANoteAndDoesNotNavigate() throws {
        let transportStore = try makeTransportStore()
        transportStore.debugSeed(snapshot: snapshot()) // "p1/s1" only — event points elsewhere
        let activityStore = ActivityStore(persistence: FakeActivityEventPersisting())
        activityStore.ingest([event(projectId: "p1", sessionId: "long-gone")], tabSelected: false)
        let router = AppRouter(pairingStore: PairingStore())
        router.selectedTab = .activity
        let model = ActivityFeedModel(activityStore: activityStore, transportStore: transportStore, router: router)

        model.handleTap(eventId: Wire.EventId("evt1"))

        #expect(router.selectedTab == .activity, "should not switch tabs for a dead session")
        #expect(router.pendingDeepLink == nil)
        #expect(model.deadSessionNote != nil)
    }

    @Test func dismissDeadSessionNoteClearsIt() throws {
        let transportStore = try makeTransportStore()
        let activityStore = ActivityStore(persistence: FakeActivityEventPersisting())
        activityStore.ingest([event(projectId: "nope", sessionId: "nope")], tabSelected: false)
        let router = AppRouter(pairingStore: PairingStore())
        let model = ActivityFeedModel(activityStore: activityStore, transportStore: transportStore, router: router)

        model.handleTap(eventId: Wire.EventId("evt1"))
        #expect(model.deadSessionNote != nil)
        model.dismissDeadSessionNote()
        #expect(model.deadSessionNote == nil)
    }
}
