//
//  ActivityFeedView.swift
//  FlightDeckRemote
//
//  Activity tab (PRD §5.7): a chronological (newest-first) feed of status
//  events for the paired Mac — finished / needs-input / error — each
//  tappable to deep-link straight to the agent. Cells render through the
//  DesignSystem `NotificationCell` for the two statuses it was built for
//  (needs-input orange / finished green); errors get a locally-styled
//  red-tinted row (`ActivityErrorCell` below), since `NotificationCell.Kind`
//  is deliberately closed to just those two (see its doc comment) and this
//  feature doesn't own that file.
//
//  Marks the feed viewed (clears the unread badge) as soon as it appears —
//  `MainTabView` also does this on tab *selection*, so this additionally
//  covers the case where the tab is already selected when new events land
//  mid-session (e.g. returning from the background).
//

import SwiftUI

struct ActivityFeedView: View {
    var transportStore: TransportStore
    var activityStore: ActivityStore
    var router: AppRouter

    @State private var model: ActivityFeedModel

    init(transportStore: TransportStore, activityStore: ActivityStore, router: AppRouter) {
        self.transportStore = transportStore
        self.activityStore = activityStore
        self.router = router
        _model = State(wrappedValue: ActivityFeedModel(
            activityStore: activityStore, transportStore: transportStore, router: router))
    }

    var body: some View {
        TimelineView(.periodic(from: .now, by: 30)) { context in
            let nowMs = Int64(context.date.timeIntervalSince1970 * 1000)
            let cells = model.cellViewModels(nowMs: nowMs)

            ZStack(alignment: .top) {
                content(cells: cells)

                if let note = model.deadSessionNote {
                    deadSessionToast(note)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .task { model.markViewed() }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("ActivityFeedView")
    }

    @ViewBuilder
    private func content(cells: [ActivityCellViewModel]) -> some View {
        if cells.isEmpty {
            emptyState
        } else {
            ScrollView {
                LazyVStack(spacing: Theme.Spacing.md) {
                    ForEach(cells) { cell in
                        Button {
                            model.handleTap(eventId: cell.id)
                        } label: {
                            ActivityCell(cell: cell)
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("activity-cell-\(cell.id.rawValue)")
                    }
                }
                .padding(Theme.Spacing.lg)
            }
        }
    }

    private var emptyState: some View {
        VStack(spacing: Theme.Spacing.md) {
            Image(systemName: "bell.badge")
                .font(.system(size: 40))
                .foregroundStyle(Theme.textDim)
            Text("No activity yet")
                .typography(Typography.headline)
                .foregroundStyle(Theme.textPrimary)
            Text("Status updates from your agents will show up here.")
                .typography(Typography.callout)
                .foregroundStyle(Theme.textMuted)
                .multilineTextAlignment(.center)
        }
        .padding(Theme.Spacing.xxl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityIdentifier("activity-empty-state")
    }

    private func deadSessionToast(_ message: String) -> some View {
        Text(message)
            .typography(Typography.callout)
            .foregroundStyle(Theme.textPrimary)
            .padding(.horizontal, Theme.Spacing.lg)
            .padding(.vertical, Theme.Spacing.sm)
            .background(Theme.bgRaised, in: RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
            .padding(.top, Theme.Spacing.lg)
            .transition(.move(edge: .top).combined(with: .opacity))
            .accessibilityIdentifier("activity-dead-session-note")
            .task(id: message) {
                try? await Task.sleep(for: .milliseconds(2_500))
                if model.deadSessionNote == message {
                    model.dismissDeadSessionNote()
                }
            }
    }
}

/// Renders one feed row, dispatching to the DesignSystem `NotificationCell`
/// for needs-input/finished, or the local red-tinted style for errors.
private struct ActivityCell: View {
    var cell: ActivityCellViewModel

    var body: some View {
        switch cell.kind {
        case .needsInput:
            NotificationCell(kind: .needsInput, title: cell.title, message: cell.message, projectTag: cell.projectTag)
        case .finished:
            NotificationCell(kind: .finished, title: cell.title, message: cell.message, projectTag: cell.projectTag)
        case .error:
            ActivityErrorCell(title: cell.title, message: cell.message, projectTag: cell.projectTag)
        }
    }
}

/// A red-tinted row matching `NotificationCell`'s exact layout/shape, for the
/// one status it doesn't cover (errors). Deliberately mirrors that view's
/// structure rather than generalizing it — `NotificationCell.Kind` is
/// consumed read-only here (DesignSystem is a sibling concern).
private struct ActivityErrorCell: View {
    var title: String
    var message: String
    var projectTag: String

    var body: some View {
        HStack(alignment: .top, spacing: 0) {
            RoundedRectangle(cornerRadius: 2, style: .continuous)
                .fill(Theme.statusRed)
                .frame(width: 4)
                .padding(.vertical, 2)

            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline) {
                    Text(title)
                        .typography(Typography.headline)
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)
                    Spacer(minLength: Theme.Spacing.sm)
                    StatusPill(label: "error", color: Theme.statusRed)
                }

                Text(message)
                    .typography(Typography.body)
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(2)

                Text(projectTag)
                    .typography(Typography.monoSmall)
                    .foregroundStyle(Theme.textDim)
            }
            .padding(Theme.Spacing.lg)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.bgCard, in: RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
        .accessibilityIdentifier("notification-cell-error")
    }
}

#Preview {
    ActivityFeedView(
        transportStore: TransportStoreFactory.makeDefault(arguments: []),
        activityStore: .makeDefault(arguments: ["-uitest-fixture-activity"]),
        router: AppRouter(pairingStore: PairingStore())
    )
}
