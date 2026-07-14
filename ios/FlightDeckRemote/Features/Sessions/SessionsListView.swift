//
//  SessionsListView.swift
//  FlightDeckRemote
//
//  PRD §5.2 Agent sessions list: back-to-Projects, project name + dot +
//  agent count, one card per session (name, agent type, status
//  idle/working-spinner/"NEEDS YOU" pill, compact Geist Mono git indicators,
//  running time, a waiting agent's question preview), sorted needs-input
//  first, then working, then manual, then idle, and a primary "New agent
//  session" CTA that reuses the FAB's existing sheet slot
//  (`MainTabView.isPresentingNewAgentSheet`) rather than rebuilding it.
//
//  Binds `TransportStore.snapshot` (Consume: Transport) for the one project
//  named `projectId`; tapping a session pushes `.chat(projectId, sessionId)`
//  onto `ProjectsNavModel.path` — Chat itself is a sibling placeholder today.
//

import SwiftUI

struct SessionsListView: View {
    var projectId: Wire.ProjectId
    var transportStore: TransportStore
    var nav: ProjectsNavModel
    var isPresentingNewAgentSheet: Binding<Bool>

    private var project: Wire.ProjectState? {
        transportStore.snapshot?.projects.first { $0.projectId == projectId }
    }

    private var sortedSessions: [Wire.SessionState] {
        SessionSort.sorted(project?.sessions ?? [])
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            content
            if project != nil {
                newAgentCTA
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep)
        .navigationBarBackButtonHidden(true)
        .toolbar(.hidden, for: .navigationBar)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("SessionsListView")
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            backButton

            HStack(spacing: Theme.Spacing.sm) {
                if let project {
                    StatusDot(status: RollupModel.viewModel(for: project).dot.agentStatus, size: .small)
                    Text(project.name)
                        .typography(Typography.title)
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)
                    Spacer(minLength: Theme.Spacing.sm)
                    Text(agentCountLabel(project.sessions.count))
                        .typography(Typography.callout)
                        .foregroundStyle(Theme.textMuted)
                } else {
                    Text("Agent sessions")
                        .typography(Typography.title)
                        .foregroundStyle(Theme.textPrimary)
                }
            }
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.top, Theme.Spacing.lg)
        .padding(.bottom, Theme.Spacing.md)
    }

    private var backButton: some View {
        Button {
            if !nav.path.isEmpty { nav.path.removeLast() }
        } label: {
            HStack(spacing: 4) {
                Image(systemName: "chevron.left")
                    .font(.system(size: 14, weight: .semibold))
                Text("Projects")
                    .typography(Typography.callout)
            }
            .foregroundStyle(Theme.accent)
        }
        .accessibilityIdentifier("sessions-back-to-projects")
    }

    private func agentCountLabel(_ count: Int) -> String {
        count == 1 ? "1 agent" : "\(count) agents"
    }

    // MARK: - Content

    @ViewBuilder
    private var content: some View {
        if project == nil {
            emptyState
        } else if sortedSessions.isEmpty {
            noSessionsState
        } else {
            sessionList
        }
    }

    private var sessionList: some View {
        ScrollView {
            LazyVStack(spacing: Theme.Spacing.md) {
                ForEach(sortedSessions, id: \.sessionId) { session in
                    sessionCard(session)
                }
            }
            .padding(Theme.Spacing.lg)
        }
    }

    private func sessionCard(_ session: Wire.SessionState) -> some View {
        Button {
            nav.path.append(.chat(projectId: projectId.rawValue, sessionId: session.sessionId.rawValue))
        } label: {
            VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
                HStack(spacing: Theme.Spacing.sm) {
                    StatusDot(status: session.status.agentStatus, size: .small)
                    Text(session.name)
                        .typography(Typography.headline)
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)
                    Spacer(minLength: Theme.Spacing.sm)
                    statusTrailing(session.status)
                }

                HStack(spacing: Theme.Spacing.sm) {
                    Text(session.agentType.displayName)
                        .typography(Typography.caption)
                        .foregroundStyle(Theme.textMuted)
                    Text("·")
                        .foregroundStyle(Theme.textDim)
                    GitIndicatorText(kind: .from(session.git))
                    Text("·")
                        .foregroundStyle(Theme.textDim)
                    Text(RunningTime.format(seconds: session.runningTimeSecs))
                        .typography(Typography.monoSmall)
                        .foregroundStyle(Theme.textDim)
                }

                if session.status == .needsInput, let question = session.pendingQuestion {
                    Text(question)
                        .typography(Typography.callout)
                        .foregroundStyle(Theme.statusNeedsInput)
                        .lineLimit(2)
                        .accessibilityIdentifier("session-pending-question-\(session.sessionId.rawValue)")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Theme.Spacing.lg)
            .cardStyle(accent: session.status == .needsInput ? Theme.statusNeedsInput : nil)
        }
        .buttonStyle(.card)
        .accessibilityIdentifier("session-card-\(session.sessionId.rawValue)")
    }

    @ViewBuilder
    private func statusTrailing(_ status: Wire.AgentStatus) -> some View {
        switch status {
        case .working:
            HStack(spacing: 6) {
                WorkingSpinner(size: 14)
                StatusPill.status(status.agentStatus)
            }
        case .needsInput:
            StatusPill.status(status.agentStatus, filled: true)
        case .idle, .manual:
            StatusPill.status(status.agentStatus)
        }
    }

    // MARK: - New agent CTA

    /// Reuses the FAB's existing sheet slot (`MainTabView.isPresentingNewAgentSheet`)
    /// rather than presenting/rebuilding its own.
    private var newAgentCTA: some View {
        Button {
            isPresentingNewAgentSheet.wrappedValue = true
        } label: {
            Text("New agent session")
                .typography(Typography.bodyMedium)
                .foregroundStyle(Theme.bgDeep)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.md)
                .background(Theme.accent, in: RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.bottom, Theme.Spacing.lg)
        .padding(.top, Theme.Spacing.xs)
        .accessibilityIdentifier("new-agent-session-cta")
    }

    // MARK: - Empty states

    private var emptyState: some View {
        VStack(spacing: Theme.Spacing.md) {
            Image(systemName: "questionmark.folder")
                .font(.system(size: 40))
                .foregroundStyle(Theme.textDim)
            Text(transportStore.snapshot == nil ? "Waiting for desktop…" : "Project not found")
                .typography(Typography.headline)
                .foregroundStyle(Theme.textPrimary)
            Text(transportStore.snapshot == nil
                 ? "Open FlightDeck on your Mac to see this project's sessions."
                 : "This project may have been closed on the desktop.")
                .typography(Typography.callout)
                .foregroundStyle(Theme.textMuted)
                .multilineTextAlignment(.center)
        }
        .padding(Theme.Spacing.xxl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityIdentifier("sessions-empty-state")
    }

    private var noSessionsState: some View {
        VStack(spacing: Theme.Spacing.sm) {
            Text("No agent sessions yet")
                .typography(Typography.callout)
                .foregroundStyle(Theme.textMuted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityIdentifier("sessions-no-sessions-state")
    }
}

#Preview {
    NavigationStack {
        SessionsListView(
            projectId: Wire.ProjectId(Wire.StateSnapshot.FixtureIds.flightdeck),
            transportStore: {
                let store = TransportStoreFactory.makeDefault(arguments: [])
                #if DEBUG
                store.debugSeed(snapshot: .uiTestFixture)
                #endif
                return store
            }(),
            nav: ProjectsNavModel(),
            isPresentingNewAgentSheet: .constant(false)
        )
    }
}
