//
//  GitIndicatorMappingTests.swift
//  FlightDeckRemoteTests
//
//  Covers `GitIndicatorText.Kind.from(_:)` (PRD §5.2/§11): `no-upstream`,
//  `clean`, and the diff form with/without a `drift:N` suffix.
//

import Testing
@testable import FlightDeckRemote

struct GitIndicatorMappingTests {

    private func git(
        added: UInt32 = 0, modified: UInt32 = 0, removed: UInt32 = 0,
        drift: UInt32 = 0, hasUpstream: Bool = true
    ) -> Wire.GitIndicators {
        Wire.GitIndicators(
            branch: "main", added: added, modified: modified, removed: removed,
            ahead: 0, behind: 0, drift: drift, hasUpstream: hasUpstream)
    }

    @Test func noUpstreamWinsRegardlessOfDiff() {
        #expect(GitIndicatorText.Kind.from(git(modified: 3, hasUpstream: false)) == .noUpstream)
    }

    @Test func cleanWhenNoDiffAndNoDrift() {
        #expect(GitIndicatorText.Kind.from(git()) == .clean)
    }

    @Test func diffWithDriftSuffix() {
        #expect(GitIndicatorText.Kind.from(git(modified: 3, drift: 2))
                == .diff(added: 0, modified: 3, deleted: 0, drift: 2))
    }

    @Test func diffWithoutDrift() {
        #expect(GitIndicatorText.Kind.from(git(added: 12, modified: 4))
                == .diff(added: 12, modified: 4, deleted: 0, drift: 0))
    }

    @Test func nonZeroDriftIsNeverClean() {
        #expect(GitIndicatorText.Kind.from(git(drift: 2)) == .diff(added: 0, modified: 0, deleted: 0, drift: 2))
    }
}
