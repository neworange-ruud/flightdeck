//
//  CustomTabBar.swift
//  FlightDeckRemote
//
//  Custom bottom tab bar: Feed · Projects · [+ FAB, center] · Shell ·
//  Settings. Hand-built rather than SwiftUI's `TabView` because the
//  center item is a raised, non-tab FAB that *presents* the New-agent flow
//  instead of switching displayed content — `TabView` has no seam for a
//  center item that behaves differently from its other tabs.
//
//  Styled entirely from DesignSystem tokens: `Theme.bgRaised` background, a
//  hairline top border, muted inactive / `textPrimary` active tab color, SF
//  Symbol icons (Shell uses a terminal glyph).
//

import SwiftUI

private struct TabBarHeightKey: EnvironmentKey {
    static let defaultValue: CGFloat = 0
}

extension EnvironmentValues {
    /// The measured height of the custom bottom tab bar, published by
    /// `MainTabView` down through `tabContent`. Screens pushed inside the
    /// Projects/Feed `NavigationStack`s do NOT inherit the tab bar's
    /// `.safeAreaInset` (a NavigationStack does not propagate an ancestor's
    /// bottom safe-area inset to nav-bar-hidden pushed destinations), so a
    /// bottom-pinned control there (e.g. `SessionsListView`'s "New agent
    /// session" CTA) would render *underneath* the bar and become unhittable.
    /// Those screens read this value and reserve matching bottom space so the
    /// control sits above the bar. Environment values DO propagate into pushed
    /// destinations, which `.safeAreaInset` does not.
    var tabBarHeight: CGFloat {
        get { self[TabBarHeightKey.self] }
        set { self[TabBarHeightKey.self] = newValue }
    }
}

struct CustomTabBar: View {
    var selectedTab: AppTab
    /// Number of unread FEED rows (remote-control-fa8) — the badge that used
    /// to live on the now-removed Activity tab now rides the Feed tab.
    var unreadFeedCount: Int
    var onSelectTab: (AppTab) -> Void
    var onTapFAB: () -> Void

    /// Tabs either side of the center FAB, in display order. `.feed` leads: the
    /// unified, attention-first multi-machine view is the primary destination
    /// (remote-control-fa8 folded the old Activity tab into it), ahead of the
    /// single-machine `.projects` tab.
    private let leadingTabs: [AppTab] = [.feed, .projects]
    private let trailingTabs: [AppTab] = [.shell, .settings]

    var body: some View {
        HStack(spacing: 0) {
            ForEach(leadingTabs) { tab in
                tabButton(tab)
            }

            fabButton

            ForEach(trailingTabs) { tab in
                tabButton(tab)
            }
        }
        .padding(.horizontal, Theme.Spacing.sm)
        .padding(.top, Theme.Spacing.sm)
        .padding(.bottom, Theme.Spacing.xs)
        .background(alignment: .top) {
            Theme.bgRaised
                .overlay(alignment: .top) {
                    Rectangle()
                        .fill(Theme.textDim.opacity(0.4))
                        .frame(height: 0.5)
                }
                .ignoresSafeArea(edges: .bottom)
        }
        // `.contain` first — same reason as the tab buttons below: an
        // identifier on a plain container would propagate onto every element
        // inside the bar and clobber their identifiers.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("tab-bar")
    }

    private func tabButton(_ tab: AppTab) -> some View {
        let isSelected = tab == selectedTab
        return Button {
            onSelectTab(tab)
        } label: {
            VStack(spacing: Theme.Spacing.xxs) {
                ZStack(alignment: .topTrailing) {
                    Image(systemName: tab.systemImage)
                        .font(.system(size: 20, weight: .semibold))
                        .frame(height: 22)

                    if tab == .feed && unreadFeedCount > 0 {
                        Circle()
                            .fill(Theme.statusNeedsInput)
                            .frame(width: 8, height: 8)
                            .offset(x: 9, y: -2)
                            // Make the badge its own element so its
                            // identifier doesn't propagate onto the
                            // enclosing tab button.
                            .accessibilityElement()
                            .accessibilityIdentifier("tab-feed-unread-badge")
                    }
                }
                Text(tab.title)
                    .typography(Typography.caption)
            }
            .foregroundStyle(isSelected ? Theme.textPrimary : Theme.textMutedDark)
            .frame(maxWidth: .infinity)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // `.contain` keeps the button exposed as a findable container even
        // though the badge circle inside carries its own accessibility
        // identifier — without it, the badge's identifier fragments the
        // button's accessibility element and the "tab-…" identifier is lost
        // from the hierarchy (observed under XCUITest).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("tab-\(tab.rawValue)")
        .accessibilityLabel(Text(tab.title))
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    private var fabButton: some View {
        Button(action: onTapFAB) {
            Image(systemName: "plus")
                .font(.system(size: 22, weight: .bold))
                .foregroundStyle(Theme.bgDeep)
                .frame(width: 56, height: 56)
                .background(Theme.accent, in: Circle())
                .shadow(color: Theme.accent.opacity(0.5), radius: 10, y: 4)
        }
        .buttonStyle(.plain)
        .offset(y: -18)
        .frame(maxWidth: .infinity)
        .accessibilityIdentifier("tab-fab-new-agent")
        .accessibilityLabel("New agent session")
    }
}

#Preview {
    VStack {
        Spacer()
        CustomTabBar(
            selectedTab: .feed,
            unreadFeedCount: 1,
            onSelectTab: { _ in },
            onTapFAB: {}
        )
    }
    .background(Theme.bgDeep)
}
