//
//  GitStatusView.swift
//  FlightDeckRemote
//
//  The read-only git status screen (PRD §5.5: "view a session's branch,
//  changed files, ahead/behind, base, drift — read-only, frictionless").
//  Populated from `TransportStore.gitStatus[sessionId]`, folded from the
//  desktop's passively-pushed `git_status` frame (`Wire.GitStatusDetail`,
//  Transport/Protocol/Common.swift) — there is no "request status" command in
//  the wire protocol, so this view simply renders whatever the store last
//  received. There is nothing to send here, so — unlike the confirmed
//  actions — this stays reachable even while commands are paused (PRD §8:
//  reads are frictionless; only state changes are gated).
//
//  `GitStatusPresentation` (this folder) does the wire → display mapping so
//  it's unit-testable without SwiftUI; this view renders its `Rows` verbatim.
//  Presented from `SessionActionsSheet`'s "Git status" row.
//

import SwiftUI

struct GitStatusView: View {
    let sessionId: Wire.SessionId
    let sessionName: String
    var store: TransportStore?

    @Environment(\.dismiss) private var dismiss

    #if DEBUG
    /// Preview/test-only override, bypassing both the store and the
    /// `-uitest-fixture-git-status` seam. Never set by production code.
    var debugDetailOverride: Wire.GitStatusDetail?
    #endif

    init(sessionId: Wire.SessionId, sessionName: String, store: TransportStore? = nil) {
        self.sessionId = sessionId
        self.sessionName = sessionName
        self.store = store
    }

    private var detail: Wire.GitStatusDetail? {
        #if DEBUG
        if let debugDetailOverride { return debugDetailOverride }
        if GitDebugSeam.isFixtureGitStatus { return GitDebugSeam.fixtureDetail(sessionId: sessionId) }
        #endif
        return store?.gitStatus[sessionId]
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.Spacing.lg) {
                    if let detail {
                        content(for: GitStatusPresentation.present(detail))
                    } else {
                        emptyState
                    }
                }
                .padding(Theme.Spacing.lg)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep)
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .presentationBackground(Theme.bgDeep)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("git-status-view")
    }

    // MARK: Header

    private var header: some View {
        HStack(spacing: Theme.Spacing.md) {
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text("Git status")
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textMuted)
                Text(sessionName)
                    .typography(Typography.headline)
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)
            }
            Spacer(minLength: Theme.Spacing.sm)
            Button("Done") { dismiss() }
                .typography(Typography.callout)
                .foregroundStyle(Theme.accent)
                .accessibilityIdentifier("git-status-done")
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.top, Theme.Spacing.xl)
        .padding(.bottom, Theme.Spacing.sm)
    }

    // MARK: Content

    @ViewBuilder
    private func content(for rows: GitStatusPresentation.Rows) -> some View {
        summaryCard(rows)
        filesSection(rows)
    }

    private func summaryCard(_ rows: GitStatusPresentation.Rows) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            summaryRow(label: "Branch", value: rows.branch, identifier: "git-status-branch")
            summaryRow(label: "Base", value: rows.baseBranch, identifier: "git-status-base")
            summaryRow(label: "Ahead / behind", value: rows.aheadBehindText,
                      identifier: "git-status-ahead-behind")
            if let driftText = rows.driftText {
                summaryRow(label: "Drift", value: driftText, identifier: "git-status-drift")
            }
        }
        .padding(Theme.Spacing.lg)
        .frame(maxWidth: .infinity, alignment: .leading)
        .cardStyle()
    }

    private func summaryRow(label: String, value: String, identifier: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label.uppercased())
                .typography(Typography.captionBold)
                .foregroundStyle(Theme.textDim)
            Spacer(minLength: Theme.Spacing.md)
            Text(value)
                .typography(Typography.mono)
                .foregroundStyle(Theme.textPrimary)
                .lineLimit(1)
                .accessibilityIdentifier(identifier)
        }
    }

    @ViewBuilder
    private func filesSection(_ rows: GitStatusPresentation.Rows) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            Text("CHANGED FILES")
                .typography(Typography.captionBold)
                .foregroundStyle(Theme.textDim)
            if rows.isClean {
                Text("Clean — no uncommitted changes.")
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.textMuted)
                    .accessibilityIdentifier("git-status-clean")
            } else {
                VStack(spacing: 0) {
                    ForEach(Array(rows.files.enumerated()), id: \.element.id) { index, file in
                        if index > 0 {
                            Divider().overlay(Theme.bgDeep)
                        }
                        fileRow(file)
                    }
                }
                .background(Theme.bgCard, in: RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
            }
        }
    }

    private func fileRow(_ file: GitStatusPresentation.FileRow) -> some View {
        HStack(spacing: Theme.Spacing.md) {
            Text(GitStatusPresentation.shortLabel(for: file.status))
                .typography(Typography.monoMedium)
                .foregroundStyle(color(for: file.status))
                .frame(width: 20)
            Text(file.path)
                .typography(Typography.mono)
                .foregroundStyle(Theme.textPrimary)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: Theme.Spacing.sm)
            if file.addedLines > 0 || file.removedLines > 0 {
                Text(lineDeltaText(file))
                    .typography(Typography.monoSmall)
                    .foregroundStyle(Theme.textDim)
            }
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.vertical, Theme.Spacing.sm)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("git-file-row")
    }

    private func lineDeltaText(_ file: GitStatusPresentation.FileRow) -> String {
        var parts: [String] = []
        if file.addedLines > 0 { parts.append("+\(file.addedLines)") }
        if file.removedLines > 0 { parts.append("-\(file.removedLines)") }
        return parts.joined(separator: " ")
    }

    private func color(for status: Wire.GitFileStatus) -> Color {
        switch status {
        case .added, .untracked: Theme.statusIdle
        case .modified: Theme.statusManual
        case .deleted: Theme.statusWorking
        case .renamed: Theme.textMuted
        }
    }

    private var emptyState: some View {
        VStack(spacing: Theme.Spacing.md) {
            Image(systemName: "arrow.triangle.branch")
                .font(.system(size: 36))
                .foregroundStyle(Theme.textMuted)
            Text("No git status yet")
                .typography(Typography.headline)
                .foregroundStyle(Theme.textPrimary)
            Text("The desktop hasn't sent this session's git status.")
                .typography(Typography.callout)
                .foregroundStyle(Theme.textMuted)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(Theme.Spacing.xxl)
        .accessibilityIdentifier("git-status-empty")
    }
}

#if DEBUG
#Preview {
    let sessionId = Wire.SessionId("sess_fix_login")
    var view = GitStatusView(sessionId: sessionId, sessionName: "fix-login")
    view.debugDetailOverride = GitDebugSeam.fixtureDetail(sessionId: sessionId)
    return Color.black.sheet(isPresented: .constant(true)) {
        view
    }
    .preferredColorScheme(.dark)
}
#endif
