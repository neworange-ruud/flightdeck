//
//  ChatViewModelTests.swift
//  FlightDeckRemoteTests
//
//  Unit tests for `ChatViewModel`: pill expand/collapse state, the
//  fixture-backed transcript, pagination gating + `loadEarlier`, and the
//  pending-permission-prompt detection that drives entry scroll/highlight.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@MainActor
@Suite struct ChatViewModelTests {

    private func fixtureModel() -> ChatViewModel {
        let model = ChatViewModel(projectId: Wire.ProjectId("p1"),
                                  sessionId: Wire.SessionId("s1"))
        model.loadFixture()
        return model
    }

    @Test func fixtureSeedsProseActivityAndPermission() {
        let model = fixtureModel()
        #expect(model.items.count == ChatFixtures.items().count)

        let kinds = model.items.map { item -> String in
            switch item {
            case .userMessage: "user"
            case .agentMessage: "agent"
            case .activity: "activity"
            case .permissionPrompt: "permission"
            }
        }
        #expect(kinds.contains("user"))
        #expect(kinds.contains("agent"))
        #expect(kinds.contains("activity"))
        #expect(kinds.contains("permission"))
    }

    @Test func pillExpandStateToggles() {
        let model = fixtureModel()
        let pill = model.items.first { if case .activity = $0 { return true }; return false }!
        let id = pill.itemId

        #expect(model.isExpanded(id) == false)
        model.toggleExpanded(id)
        #expect(model.isExpanded(id))
        model.toggleExpanded(id)
        #expect(model.isExpanded(id) == false)
    }

    @Test func pendingPromptDetectedAndNeedsInput() {
        let model = fixtureModel()
        #expect(model.pendingPromptItemId != nil)
        #expect(model.isNeedsInput)
        // The pending id points at a permission-prompt item.
        let pendingItem = model.items.first { $0.itemId == model.pendingPromptItemId }
        #expect(pendingItem?.permissionPromptId != nil)
    }

    @Test func canLoadEarlierReflectsFixtureWindow() {
        let model = fixtureModel()
        #expect(model.fromIndex == ChatFixtures.fromIndex)
        #expect(model.canLoadEarlier) // fixture from_index > 0
    }

    @Test func loadEarlierPrependsAndReachesHead() {
        let model = fixtureModel()
        let before = model.items.count
        let earlierCount = ChatFixtures.earlierItems().count

        model.loadEarlier()

        #expect(model.items.count == before + earlierCount)
        #expect(model.fromIndex == 0)
        #expect(model.canLoadEarlier == false) // head reached, no more paging
        // Earlier items are prepended (order preserved).
        #expect(model.items.first?.itemId == ChatFixtures.earlierItems().first?.itemId)
    }

    @Test func surfaceDefaultsToAgent() {
        let model = fixtureModel()
        #expect(model.surface == .agent)
    }

    @Test func headerMetadataFromFixtureSession() {
        let model = fixtureModel()
        #expect(model.sessionName == "fix-login")
        #expect(model.agentType == .claudeCode)
    }
}
