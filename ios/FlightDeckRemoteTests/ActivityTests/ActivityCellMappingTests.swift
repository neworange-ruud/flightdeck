//
//  ActivityCellMappingTests.swift
//  FlightDeckRemoteTests
//
//  Covers the pure `Wire.AgentEvent` → cell view-model mapping (PRD §5.7):
//  kind mapping (needs-input/finished/error), message composition per
//  `EventKind`, and the relative-time formatting boundaries.
//

import Testing
@testable import FlightDeckRemote

struct ActivityCellMappingTests {

    private func deepLink() -> Wire.DeepLink {
        Wire.DeepLink(projectId: Wire.ProjectId("p1"), sessionId: Wire.SessionId("s1"), itemId: nil)
    }

    // MARK: - Kind mapping

    @Test func mapsEachEventKindToItsCellKind() {
        #expect(ActivityCellMapper.kind(for: .needsInput(preview: "x")) == .needsInput)
        #expect(ActivityCellMapper.kind(for: .finished(summary: "x", filesChanged: 0, readyToPush: false)) == .finished)
        #expect(ActivityCellMapper.kind(for: .error(message: "x")) == .error)
    }

    // MARK: - Message composition

    @Test func needsInputMessageIsThePreviewVerbatim() {
        #expect(ActivityCellMapper.message(for: .needsInput(preview: "Run it?")) == "Run it?")
    }

    @Test func errorMessageIsTheErrorTextVerbatim() {
        #expect(ActivityCellMapper.message(for: .error(message: "boom")) == "boom")
    }

    @Test func finishedMessageWithNoFilesAndNotReadyIsJustTheSummary() {
        let message = ActivityCellMapper.message(for: .finished(summary: "Done", filesChanged: 0, readyToPush: false))
        #expect(message == "Done")
    }

    @Test func finishedMessageIncludesSingularFileCount() {
        let message = ActivityCellMapper.message(for: .finished(summary: "Done", filesChanged: 1, readyToPush: false))
        #expect(message == "Done · 1 file changed")
    }

    @Test func finishedMessageIncludesPluralFileCountAndReadyToPush() {
        let message = ActivityCellMapper.message(for: .finished(summary: "Done", filesChanged: 3, readyToPush: true))
        #expect(message == "Done · 3 files changed · ready to push")
    }

    // MARK: - Relative time

    @Test func relativeTimeJustNowUnderAMinute() {
        #expect(ActivityCellMapper.relativeTimeString(fromMs: 0, nowMs: 0) == "just now")
        #expect(ActivityCellMapper.relativeTimeString(fromMs: 0, nowMs: 59_000) == "just now")
    }

    @Test func relativeTimeMinutesBoundary() {
        #expect(ActivityCellMapper.relativeTimeString(fromMs: 0, nowMs: 60_000) == "1m ago")
        #expect(ActivityCellMapper.relativeTimeString(fromMs: 0, nowMs: 59 * 60_000) == "59m ago")
    }

    @Test func relativeTimeHoursBoundary() {
        #expect(ActivityCellMapper.relativeTimeString(fromMs: 0, nowMs: 60 * 60_000) == "1h ago")
        #expect(ActivityCellMapper.relativeTimeString(fromMs: 0, nowMs: 23 * 60 * 60_000) == "23h ago")
    }

    @Test func relativeTimeDaysBoundary() {
        #expect(ActivityCellMapper.relativeTimeString(fromMs: 0, nowMs: 24 * 60 * 60_000) == "1d ago")
        #expect(ActivityCellMapper.relativeTimeString(fromMs: 0, nowMs: 3 * 24 * 60 * 60_000) == "3d ago")
    }

    @Test func relativeTimeNeverGoesNegativeForFutureTimestamps() {
        // A clock skew edge case: the event's timestamp is (implausibly)
        // after `now` — must not crash or format a negative duration.
        #expect(ActivityCellMapper.relativeTimeString(fromMs: 1_000, nowMs: 0) == "just now")
    }

    // MARK: - Full view model

    @Test func viewModelComposesProjectTagFromNameAndRelativeTime() {
        let event = Wire.AgentEvent(
            eventId: Wire.EventId("e1"),
            kind: .needsInput(preview: "Run it?"),
            deepLink: deepLink(),
            occurredAtMs: 0,
            title: "fix-login needs your input")
        let vm = ActivityCellMapper.viewModel(for: event, projectName: "flightdeck", nowMs: 60_000)

        #expect(vm.id == Wire.EventId("e1"))
        #expect(vm.kind == .needsInput)
        #expect(vm.title == "fix-login needs your input")
        #expect(vm.message == "Run it?")
        #expect(vm.projectTag == "flightdeck · 1m ago")
    }
}
