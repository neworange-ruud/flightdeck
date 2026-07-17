//
//  GitDebugSeam.swift
//  FlightDeckRemote
//
//  DEBUG-only launch-argument seam so UI tests (and previews) can render the
//  git status screen and the merge-back guard note deterministically without
//  a live desktop — mirrors `ShellDebugSeam` (Features/Shell/ShellView.swift):
//  the wire protocol's `git_status` frame is push-only (there is no
//  "request status" command), so `GitStatusView`/`SessionActionsSheet` read
//  straight from `TransportStore.gitStatus`; this seam supplies a
//  deterministic `Wire.GitStatusDetail` in its place when
//  `-uitest-fixture-git-status` is present, bypassing the store entirely.
//
//  Usage: `-uitest-fixture-git-status` (combine with `-uitest-fixture-snapshot`
//  for the session list and `-uitest-linkstate <state>` to exercise the
//  paused/blocked pull-base and merge-back cases).
//

#if DEBUG
import Foundation

enum GitDebugSeam {
    static var isFixtureGitStatus: Bool {
        ProcessInfo.processInfo.arguments.contains("-uitest-fixture-git-status")
    }

    /// A representative dirty-with-drift status: a couple of ahead/behind
    /// commits, base drift, and one change of each file status — enough to
    /// exercise every changed-files row kind and the merge-back guard note in
    /// one fixture.
    static func fixtureDetail(sessionId: Wire.SessionId) -> Wire.GitStatusDetail {
        Wire.GitStatusDetail(
            sessionId: sessionId,
            branch: "flightdeck/fix-login",
            baseBranch: "main",
            hasUpstream: true,
            ahead: 2,
            behind: 1,
            drift: 3,
            files: [
                Wire.GitFileChange(path: "Sources/Login.swift", status: .modified,
                                   addedLines: 12, removedLines: 4),
                Wire.GitFileChange(path: "Sources/LoginTests.swift", status: .added,
                                   addedLines: 40, removedLines: 0),
                Wire.GitFileChange(path: "Sources/OldFlow.swift", status: .deleted,
                                   addedLines: 0, removedLines: 30),
            ])
    }
}
#endif
