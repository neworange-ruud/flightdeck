//
//  ComponentGallery.swift
//  FlightDeckRemote
//
//  Scrollable gallery rendering every DesignSystem component in all its
//  states — this is the design-system task's acceptance surface. Reachable
//  in DEBUG builds via a floating launcher button (see
//  ComponentGalleryDebugEntry.swift), and by unit/UI tests directly.
//
//  IMPORTANT: `.accessibilityIdentifier("component-gallery")` is applied to
//  the outer container *without* `.accessibilityElement(children: .contain)`
//  so descendant identifiers (status-dot-*, working-spinner, etc.) stay
//  independently queryable by XCUITest.
//

import SwiftUI

struct ComponentGallery: View {

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.Spacing.xxl) {
                colorSection
                typographySection
                statusDotSection
                spinnerSection
                pillSection
                cardSection
                gitIndicatorSection
                notificationCellSection
            }
            .padding(Theme.Spacing.lg)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .background(Theme.bgDeep.ignoresSafeArea())
        .accessibilityIdentifier("component-gallery")
    }

    // MARK: - Colors

    private var colorSection: some View {
        GallerySection(title: "Colors") {
            GalleryFlow {
                swatch("bgDeep", Theme.bgDeep)
                swatch("bgField", Theme.bgField)
                swatch("bgCard", Theme.bgCard)
                swatch("bgRaised", Theme.bgRaised)
                swatch("accent", Theme.accent)
                swatch("textPrimary", Theme.textPrimary)
                swatch("textMuted", Theme.textMuted)
                swatch("textDim", Theme.textDim)
                swatch("statusWorking", Theme.statusWorking)
                swatch("statusIdle", Theme.statusIdle)
                swatch("statusNeedsInput", Theme.statusNeedsInput)
                swatch("statusManual", Theme.statusManual)
            }
        }
    }

    private func swatch(_ name: String, _ color: Color) -> some View {
        VStack(spacing: 6) {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(color)
                .frame(width: 64, height: 44)
                .overlay(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .strokeBorder(Theme.text.opacity(0.08), lineWidth: 1)
                )
            Text(name)
                .typography(Typography.monoSmall)
                .foregroundStyle(Theme.textDim)
        }
        .accessibilityIdentifier("swatch-\(name)")
    }

    // MARK: - Typography

    private var typographySection: some View {
        GallerySection(title: "Typography (\(Typography.isCustomFontAvailable ? "Geist" : "system fallback"))") {
            VStack(alignment: .leading, spacing: 10) {
                Text("Large Title").typography(Typography.largeTitle).foregroundStyle(Theme.textPrimary)
                Text("Title").typography(Typography.title).foregroundStyle(Theme.textPrimary)
                Text("Headline").typography(Typography.headline).foregroundStyle(Theme.textPrimary)
                Text("Body — the quick brown fox").typography(Typography.body).foregroundStyle(Theme.textPrimary)
                Text("Body Medium — the quick brown fox").typography(Typography.bodyMedium).foregroundStyle(Theme.textPrimary)
                Text("Callout — supporting copy").typography(Typography.callout).foregroundStyle(Theme.textMuted)
                Text("CAPTION LABEL").typography(Typography.captionBold).foregroundStyle(Theme.textDim)
                Text("mono — +12 ~4 -1 drift:2").typography(Typography.mono).foregroundStyle(Theme.textPrimary)
                Text("monoSmall — a1b2c3d clean").typography(Typography.monoSmall).foregroundStyle(Theme.textDim)
            }
            .accessibilityIdentifier("typography-samples")
        }
    }

    // MARK: - StatusDot

    private var statusDotSection: some View {
        GallerySection(title: "StatusDot") {
            VStack(alignment: .leading, spacing: 16) {
                dotRow(title: "small", size: .small)
                dotRow(title: "large", size: .large)
            }
        }
    }

    private func dotRow(title: String, size: StatusDot.Size) -> some View {
        HStack(spacing: 20) {
            labeledDot(.working, size: size)
            labeledDot(.idle, size: size)
            labeledDot(.needsInput, size: size)
            labeledDot(.manual(), size: size)
        }
    }

    private func labeledDot(_ status: AgentStatus, size: StatusDot.Size) -> some View {
        VStack(spacing: 6) {
            StatusDot(status: status, size: size)
            Text(status.label).typography(Typography.monoSmall).foregroundStyle(Theme.textDim)
        }
    }

    // MARK: - WorkingSpinner

    private var spinnerSection: some View {
        GallerySection(title: "WorkingSpinner") {
            HStack(spacing: 24) {
                WorkingSpinner(size: 14)
                WorkingSpinner(size: 20, lineWidth: 2.5)
                WorkingSpinner(size: 28, lineWidth: 3)
            }
        }
    }

    // MARK: - StatusPill

    private var pillSection: some View {
        GallerySection(title: "StatusPill") {
            VStack(alignment: .leading, spacing: 12) {
                GalleryFlow {
                    StatusPill.status(.working)
                    StatusPill.status(.idle)
                    StatusPill.status(.needsInput)
                    StatusPill.status(.manual())
                }
                GalleryFlow {
                    StatusPill.status(.working, filled: true)
                    StatusPill.status(.idle, filled: true)
                    StatusPill.status(.needsInput, filled: true)
                    StatusPill.status(.manual(), filled: true)
                }
                StatusPill(label: "3 agents", color: Theme.textMuted)
            }
        }
    }

    // MARK: - CardStyle

    private var cardSection: some View {
        GallerySection(title: "CardStyle") {
            VStack(spacing: 14) {
                card(title: "flightdeck", subtitle: "1 needs input · 1 working · 3 agents", accent: Theme.statusNeedsInput)
                card(title: "remote-control", subtitle: "idle · 2 agents", accent: Theme.statusIdle)

                Button {
                } label: {
                    card(title: "dac", subtitle: "tap me — CardButtonStyle", accent: Theme.statusWorking)
                }
                .buttonStyle(.card)
                .accessibilityIdentifier("card-tappable-example")
            }
        }
    }

    private func card(title: String, subtitle: String, accent: Color) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).typography(Typography.headline).foregroundStyle(Theme.textPrimary)
            Text(subtitle).typography(Typography.callout).foregroundStyle(Theme.textMuted)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Theme.Spacing.lg)
        .cardStyle(accent: accent)
        .accessibilityIdentifier("card-\(title)")
    }

    // MARK: - GitIndicatorText

    private var gitIndicatorSection: some View {
        GallerySection(title: "GitIndicatorText") {
            VStack(alignment: .leading, spacing: 8) {
                GitIndicatorText(kind: .diff(modified: 3, drift: 2))
                GitIndicatorText(kind: .diff(added: 12, modified: 4))
                GitIndicatorText(kind: .clean)
                GitIndicatorText(kind: .noUpstream)
                GitIndicatorText(kind: .custom("detached @ a1b2c3d"))
            }
        }
    }

    // MARK: - NotificationCell

    private var notificationCellSection: some View {
        GallerySection(title: "NotificationCell") {
            VStack(spacing: 12) {
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
        }
    }
}

/// A titled section container used to lay out the gallery.
private struct GallerySection<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.md) {
            Text(title.uppercased())
                .typography(Typography.captionBold)
                .foregroundStyle(Theme.accent)
            content
        }
    }
}

/// A simple wrapping flow layout for pills/swatches, built on SwiftUI's
/// `Layout` protocol (iOS 16+).
private struct GalleryFlow: Layout {
    var spacing: CGFloat = 10

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let maxWidth = proposal.width ?? .infinity
        var rowWidth: CGFloat = 0
        var totalHeight: CGFloat = 0
        var rowHeight: CGFloat = 0

        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if rowWidth + size.width > maxWidth, rowWidth > 0 {
                totalHeight += rowHeight + spacing
                rowWidth = 0
                rowHeight = 0
            }
            rowWidth += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
        totalHeight += rowHeight
        return CGSize(width: maxWidth.isFinite ? maxWidth : rowWidth, height: totalHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX
        var y = bounds.minY
        var rowHeight: CGFloat = 0

        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x + size.width > bounds.maxX, x > bounds.minX {
                x = bounds.minX
                y += rowHeight + spacing
                rowHeight = 0
            }
            subview.place(at: CGPoint(x: x, y: y), proposal: .unspecified)
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}

#Preview {
    ComponentGallery()
}
