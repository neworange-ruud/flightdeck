//
//  ShellTabView.swift
//  FlightDeckRemote
//
//  The Shell TAB (PRD §5.7 bottom bar). Since a shell lives in a *session's*
//  worktree (PRD §5.4: one shell at a time per session), the tab first offers
//  a session picker (flattened from the live `TransportStore` snapshot), then
//  mounts `ShellView` for the chosen session. The last-used session is
//  remembered for the app launch, so returning to the tab reopens where you
//  left off. Empty state ("pick a session") shows when there are no sessions.
//

import SwiftUI

struct ShellTabView: View {
    var transportStore: TransportStore

    /// The picked session (nil → show the picker). Remembered per launch via
    /// the static below.
    @State private var selection: PickedSession?

    /// Last-used session for the launch (survives leaving/returning to the
    /// tab; deliberately not persisted across launches).
    @MainActor static var lastUsed: PickedSession?

    struct PickedSession: Hashable {
        let sessionId: Wire.SessionId
        let name: String
    }

    private var sessions: [PickedSession] {
        (transportStore.snapshot?.projects ?? []).flatMap { project in
            project.sessions.map { PickedSession(sessionId: $0.sessionId, name: $0.name) }
        }
    }

    var body: some View {
        Group {
            if let selection {
                shell(for: selection)
            } else {
                picker
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep)
        .onAppear { restoreOrAutoSelect() }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("ShellTabView")
    }

    // MARK: - Shell

    private func shell(for picked: PickedSession) -> some View {
        VStack(spacing: 0) {
            header(picked: picked)
            ShellView(sessionId: picked.sessionId, sessionName: picked.name, store: transportStore)
        }
    }

    private func header(picked: PickedSession) -> some View {
        HStack(spacing: Theme.Spacing.md) {
            Button {
                selection = nil
            } label: {
                Image(systemName: "chevron.left")
                    .font(.system(size: 18, weight: .semibold))
                    .foregroundStyle(Theme.textPrimary)
                    .frame(width: 32, height: 32)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("shell-picker-back")

            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text("Shell")
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textMuted)
                Text(picked.name)
                    .typography(Typography.headline)
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.vertical, Theme.Spacing.md)
        .background(Theme.bgRaised)
    }

    // MARK: - Picker

    private var picker: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Shell")
                .typography(Typography.title)
                .foregroundStyle(Theme.textPrimary)
                .padding(.horizontal, Theme.Spacing.lg)
                .padding(.top, Theme.Spacing.lg)
                .padding(.bottom, Theme.Spacing.md)

            if sessions.isEmpty {
                emptyState
            } else {
                Text("Pick a session to open a shell in its worktree.")
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.textMuted)
                    .padding(.horizontal, Theme.Spacing.lg)
                    .padding(.bottom, Theme.Spacing.md)
                ScrollView {
                    LazyVStack(spacing: Theme.Spacing.sm) {
                        ForEach(sessions, id: \.self) { session in
                            sessionRow(session)
                        }
                    }
                    .padding(.horizontal, Theme.Spacing.lg)
                }
            }
            Spacer(minLength: 0)
        }
    }

    private func sessionRow(_ session: PickedSession) -> some View {
        Button {
            pick(session)
        } label: {
            HStack(spacing: Theme.Spacing.md) {
                Image(systemName: "terminal")
                    .foregroundStyle(Theme.textMuted)
                Text(session.name)
                    .typography(Typography.bodyMedium)
                    .foregroundStyle(Theme.textPrimary)
                Spacer(minLength: 0)
                Image(systemName: "chevron.right")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.textDim)
            }
            .padding(Theme.Spacing.lg)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                    .fill(Theme.bgCard)
            )
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("shell-session-\(session.sessionId.rawValue)")
    }

    private var emptyState: some View {
        VStack(spacing: Theme.Spacing.md) {
            Image(systemName: "terminal")
                .font(.system(size: 44))
                .foregroundStyle(Theme.textMuted)
            Text("No sessions yet")
                .typography(Typography.headline)
                .foregroundStyle(Theme.textPrimary)
            Text("Start an agent session, then open a shell in its worktree.")
                .typography(Typography.callout)
                .foregroundStyle(Theme.textMuted)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(Theme.Spacing.xl)
        .accessibilityIdentifier("shell-empty")
    }

    // MARK: - Selection

    private func pick(_ session: PickedSession) {
        selection = session
        Self.lastUsed = session
    }

    private func restoreOrAutoSelect() {
        guard selection == nil else { return }
        #if DEBUG
        // UI-test fixture: land straight on the first session's shell.
        if ShellDebugSeam.isFixtureShell, let first = sessions.first {
            selection = first
            return
        }
        #endif
        if let last = Self.lastUsed, sessions.contains(last) {
            selection = last
        }
    }
}
