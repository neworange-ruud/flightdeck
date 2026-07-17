//
//  NotificationCell.swift
//  FlightDeckRemote
//
//  Lock-screen-style notification preview cell — colored left border variant
//  (orange needs-input / green finished), title + body + project tag. Built
//  for the Activity feed (PRD §5.7) to consume; standalone here so it can be
//  designed and tested independently.
//

import SwiftUI

struct NotificationCell: View {

    /// The two notification-worthy statuses (PRD §4: needs input is the
    /// urgent one; finished/idle is informational).
    enum Kind {
        case needsInput
        case finished

        var color: Color {
            switch self {
            case .needsInput: Theme.statusNeedsInput
            case .finished: Theme.statusIdle
            }
        }

        var pillLabel: String {
            switch self {
            case .needsInput: "needs you"
            case .finished: "finished"
            }
        }
    }

    var kind: Kind
    var title: String
    /// The notification's message text. (Named `message`, not `body`, since
    /// `body` is reserved for the `View` protocol's required property.)
    var message: String
    var projectTag: String

    var body: some View {
        HStack(alignment: .top, spacing: 0) {
            RoundedRectangle(cornerRadius: 2, style: .continuous)
                .fill(kind.color)
                .frame(width: 4)
                .padding(.vertical, 2)

            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline) {
                    Text(title)
                        .typography(Typography.headline)
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)
                    Spacer(minLength: Theme.Spacing.sm)
                    StatusPill(label: kind.pillLabel, color: kind.color)
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
        .accessibilityIdentifier("notification-cell-\(kind == .needsInput ? "needs-input" : "finished")")
    }
}

extension NotificationCell.Kind: Equatable {}

#Preview {
    VStack(spacing: 14) {
        NotificationCell(
            kind: .needsInput,
            title: "flightdeck · agent-3",
            message: "Ready to run `terraform apply` — approve?",
            projectTag: "flightdeck"
        )
        NotificationCell(
            kind: .finished,
            title: "remote-control · agent-1",
            message: "Finished: added StatusDot, WorkingSpinner, StatusPill components.",
            projectTag: "remote-control"
        )
    }
    .padding(20)
    .background(Theme.bgDeep)
}
