//
//  CardStyle.swift
//  FlightDeckRemote
//
//  Session/project card container: bg, subtle 1px colored border, optional
//  colored left accent bar, large corner radius, tap highlight. PRD §11
//  shape language.
//

import SwiftUI

/// Card container styling — applies background, border, corner radius, and
/// an optional colored left accent bar (e.g. the project/session's status
/// color) to any content.
struct CardStyle: ViewModifier {

    var accentColor: Color? = nil
    var radius: CGFloat = Theme.Radius.card
    var borderColor: Color = Theme.text.opacity(0.08)

    func body(content: Content) -> some View {
        content
            .padding(.leading, accentColor != nil ? Theme.Spacing.sm : 0)
            .background(Theme.bgCard, in: RoundedRectangle(cornerRadius: radius, style: .continuous))
            .overlay(alignment: .leading) {
                if let accentColor {
                    RoundedRectangle(cornerRadius: 2, style: .continuous)
                        .fill(accentColor)
                        .frame(width: 4)
                        .padding(.vertical, Theme.Spacing.md)
                        .padding(.leading, Theme.Spacing.sm)
                }
            }
            .overlay(
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .strokeBorder(borderColor, lineWidth: 1)
            )
            .clipShape(RoundedRectangle(cornerRadius: radius, style: .continuous))
    }
}

extension View {
    /// Wraps the view in the standard card container, with an optional
    /// colored left accent bar (typically a status color).
    func cardStyle(accent: Color? = nil, radius: CGFloat = Theme.Radius.card) -> some View {
        modifier(CardStyle(accentColor: accent, radius: radius))
    }
}

/// Tap-highlight `ButtonStyle` for tappable cards (session/project rows).
struct CardButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.98 : 1)
            .opacity(configuration.isPressed ? 0.82 : 1)
            .animation(.easeOut(duration: 0.15), value: configuration.isPressed)
    }
}

extension ButtonStyle where Self == CardButtonStyle {
    static var card: CardButtonStyle { CardButtonStyle() }
}

#Preview {
    VStack(spacing: 16) {
        VStack(alignment: .leading, spacing: 4) {
            Text("flightdeck").typography(Typography.headline).foregroundStyle(Theme.textPrimary)
            Text("3 agents · 1 needs input").typography(Typography.callout).foregroundStyle(Theme.textMuted)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Theme.Spacing.lg)
        .cardStyle(accent: Theme.statusNeedsInput)

        Button {
        } label: {
            VStack(alignment: .leading, spacing: 4) {
                Text("remote-control").typography(Typography.headline).foregroundStyle(Theme.textPrimary)
                Text("idle · 2 agents").typography(Typography.callout).foregroundStyle(Theme.textMuted)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Theme.Spacing.lg)
            .cardStyle(accent: Theme.statusIdle)
        }
        .buttonStyle(.card)
    }
    .padding(24)
    .background(Theme.bgDeep)
}
