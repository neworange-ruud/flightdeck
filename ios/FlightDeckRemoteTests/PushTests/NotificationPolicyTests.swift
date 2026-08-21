//
//  NotificationPolicyTests.swift
//  FlightDeckRemoteTests
//
//  The pure notification gate (PRD §5.6/§9.2): independent toggles + the
//  completion-chime sound gate + per-project mute.
//

import Testing
@testable import FlightDeckRemote

struct NotificationPolicyTests {

    @Test func needsInputPresentsWithSoundWhenEnabled() {
        let settings = NotificationSettings(agentNeedsInput: true)
        #expect(NotificationPolicy.outcome(for: PushFixtures.needsInput, settings: settings) == .present(sound: true))
    }

    @Test func needsInputSuppressedWhenToggledOff() {
        let settings = NotificationSettings(agentNeedsInput: false)
        #expect(NotificationPolicy.outcome(for: PushFixtures.needsInput, settings: settings) == .suppressed)
    }

    @Test func finishedSoundFollowsCompletionChimeIndependently() {
        // Finished ON, chime OFF → present silently.
        let noChime = NotificationSettings(agentFinished: true, completionChime: false)
        #expect(NotificationPolicy.outcome(for: PushFixtures.finished, settings: noChime) == .present(sound: false))

        // Finished ON, chime ON → present with sound.
        let chime = NotificationSettings(agentFinished: true, completionChime: true)
        #expect(NotificationPolicy.outcome(for: PushFixtures.finished, settings: chime) == .present(sound: true))
    }

    @Test func finishedSuppressedWhenToggledOff() {
        let settings = NotificationSettings(agentFinished: false)
        #expect(NotificationPolicy.outcome(for: PushFixtures.finished, settings: settings) == .suppressed)
    }

    @Test func errorFollowsFinishedToggleAndChime() {
        #expect(NotificationPolicy.outcome(
            for: PushFixtures.errored,
            settings: NotificationSettings(agentFinished: false)) == .suppressed)
        #expect(NotificationPolicy.outcome(
            for: PushFixtures.errored,
            settings: NotificationSettings(agentFinished: true, completionChime: false)) == .present(sound: false))
    }

    @Test func mutedProjectSuppressesEvenUrgentNeedsInput() {
        let settings = NotificationSettings(agentNeedsInput: true, mutedProjectIds: ["proj_1"])
        #expect(NotificationPolicy.outcome(for: PushFixtures.needsInput, settings: settings) == .suppressed)
    }

    @Test func muteIsScopedToTheProjectId() {
        // Muting a different project does not suppress this one.
        let settings = NotificationSettings(agentNeedsInput: true, mutedProjectIds: ["other"])
        #expect(NotificationPolicy.outcome(for: PushFixtures.needsInput, settings: settings) == .present(sound: true))
    }
}
