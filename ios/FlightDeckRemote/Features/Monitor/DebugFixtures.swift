//
//  DebugFixtures.swift
//  FlightDeckRemote
//
//  A realistic `Wire.StateSnapshot` fixture so the Projects/Sessions screens
//  render fully populated content in SwiftUI previews and UI tests without a
//  live desktop. Seeded via `TransportStoreFactory` when the app launches
//  with `-uitest-fixture-snapshot` (see `TransportStore.debugSeed`).
//
//  Covers every status (needs-input with a pending question, working,
//  manual, idle), all three agent types, and every `GitIndicatorText.Kind`
//  shape (`~3 drift:2`, `+12 ~4`, `clean`, `no-upstream`) across three
//  projects so both list screens have representative rows to render.
//
//  DEBUG-only: never compiled into Release.
//

#if DEBUG
import Foundation

extension Wire.StateSnapshot {
    /// Stable ids used by both the fixture and UI tests that assert against
    /// `project-card-<id>` accessibility identifiers.
    enum FixtureIds {
        static let flightdeck = "proj_flightdeck"
        static let remoteControl = "proj_remote_control"
        static let marketingSite = "proj_marketing_site"
    }

    /// The `-uitest-fixture-snapshot` fixture (see `TransportStoreFactory`).
    static var uiTestFixture: Wire.StateSnapshot {
        Wire.StateSnapshot(serverTimeMs: 1_752_400_000_000, projects: [
            flightdeckProject,
            remoteControlProject,
            marketingSiteProject,
        ])
    }

    private static var flightdeckProject: Wire.ProjectState {
        let sessions = [
            Wire.SessionState(
                sessionId: Wire.SessionId("sess_fix_login"),
                projectId: Wire.ProjectId(FixtureIds.flightdeck),
                name: "fix-login",
                agentType: .claudeCode,
                status: .needsInput,
                git: Wire.GitIndicators(
                    branch: "flightdeck/fix-login", added: 0, modified: 3, removed: 0,
                    ahead: 0, behind: 0, drift: 2, hasUpstream: true),
                runningTimeSecs: 435,
                pendingQuestion: "Should I run `terraform apply` to provision the staging bucket, or hold until you review the plan?"
            ),
            Wire.SessionState(
                sessionId: Wire.SessionId("sess_add_tests"),
                projectId: Wire.ProjectId(FixtureIds.flightdeck),
                name: "add-tests",
                agentType: .opencode,
                status: .working,
                git: Wire.GitIndicators(
                    branch: "flightdeck/add-tests", added: 12, modified: 4, removed: 0,
                    ahead: 0, behind: 0, drift: 0, hasUpstream: true),
                runningTimeSecs: 4_320,
                pendingQuestion: nil
            ),
            Wire.SessionState(
                sessionId: Wire.SessionId("sess_cleanup_warnings"),
                projectId: Wire.ProjectId(FixtureIds.flightdeck),
                name: "cleanup-warnings",
                agentType: .codex,
                status: .idle,
                git: Wire.GitIndicators(
                    branch: "flightdeck/cleanup-warnings", added: 0, modified: 0, removed: 0,
                    ahead: 0, behind: 0, drift: 0, hasUpstream: true),
                runningTimeSecs: 96,
                pendingQuestion: nil
            ),
        ]
        return Wire.ProjectState(
            projectId: Wire.ProjectId(FixtureIds.flightdeck),
            name: "flightdeck",
            rollup: rollup(for: sessions),
            sessions: sessions
        )
    }

    private static var remoteControlProject: Wire.ProjectState {
        let sessions = [
            Wire.SessionState(
                sessionId: Wire.SessionId("sess_relay_handshake"),
                projectId: Wire.ProjectId(FixtureIds.remoteControl),
                name: "relay-handshake",
                agentType: .claudeCode,
                status: .working,
                git: Wire.GitIndicators(
                    branch: "relay-handshake", added: 0, modified: 0, removed: 0,
                    ahead: 0, behind: 0, drift: 0, hasUpstream: false),
                runningTimeSecs: 45,
                pendingQuestion: nil
            ),
            Wire.SessionState(
                sessionId: Wire.SessionId("sess_update_docs"),
                projectId: Wire.ProjectId(FixtureIds.remoteControl),
                name: "update-docs",
                agentType: .codex,
                status: .manual(label: "reviewing"),
                git: Wire.GitIndicators(
                    branch: "update-docs", added: 0, modified: 0, removed: 0,
                    ahead: 0, behind: 0, drift: 0, hasUpstream: true),
                runningTimeSecs: 600,
                pendingQuestion: nil
            ),
        ]
        return Wire.ProjectState(
            projectId: Wire.ProjectId(FixtureIds.remoteControl),
            name: "remote-control",
            rollup: rollup(for: sessions),
            sessions: sessions
        )
    }

    private static var marketingSiteProject: Wire.ProjectState {
        let sessions = [
            Wire.SessionState(
                sessionId: Wire.SessionId("sess_hero_copy"),
                projectId: Wire.ProjectId(FixtureIds.marketingSite),
                name: "hero-copy",
                agentType: .claudeCode,
                status: .idle,
                git: Wire.GitIndicators(
                    branch: "hero-copy", added: 0, modified: 0, removed: 0,
                    ahead: 0, behind: 0, drift: 0, hasUpstream: true),
                runningTimeSecs: 182,
                pendingQuestion: nil
            ),
        ]
        return Wire.ProjectState(
            projectId: Wire.ProjectId(FixtureIds.marketingSite),
            name: "marketing-site",
            rollup: rollup(for: sessions),
            sessions: sessions
        )
    }

    /// Builds a `Wire.StatusRollup` the same way the local fallback does
    /// (`RollupModel`) so the fixture's dot/summary stay consistent with the
    /// real folding logic.
    private static func rollup(for sessions: [Wire.SessionState]) -> Wire.StatusRollup {
        let vm = RollupModel.rollup(sessions: sessions)
        return Wire.StatusRollup(
            dot: vm.dot, summary: vm.summary, working: vm.working, idle: vm.idle,
            needsInput: vm.needsInput, manual: vm.manual, agentCount: vm.agentCount)
    }
}
#endif
