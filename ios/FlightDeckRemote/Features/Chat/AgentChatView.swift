//
//  AgentChatView.swift
//  FlightDeckRemote
//
//  The cleaned agent-chat transcript surface (PRD §5.3 3a) — never a raw
//  terminal. It renders:
//   - a header (session name, agent type, glowing StatusDot, back);
//   - the `Agent · Shell` surface switcher (Shell disabled this task);
//   - a bottom-anchored transcript list (LazyVStack in a ScrollView) of prose,
//     collapsible activity pills, and inline permission-prompt cards, with a
//     "load earlier" affordance at the top and standard chat auto-scroll
//     (stick to newest unless the user scrolled up, with a "jump to latest"
//     affordance);
//   - an inert `ChatComposeBar` seam at the bottom.
//
//  Seams left for sibling tasks:
//   - `ChatSurface.shell` — the shell task enables the disabled Shell segment
//     and renders the real terminal (PRD §5.4).
//   - `store` — when the transport is injected into the view tree, live
//     `transcript`/`transcript_append` deltas stream straight in via the
//     view-model's store binding, and the store is the command sender + gate
//     source.
//   - `ChatComposeBar.onHoldToTalk` — the custom hold-to-talk voice task
//     (`remote-control-chat-voice-dictation`) fills this in; v1 mic uses system
//     keyboard dictation only (PRD §7).
//

import SwiftUI

struct AgentChatView: View {

    @State private var model: ChatViewModel
    private let store: TransportStore?

    #if DEBUG
    // Fixture / UI-test send path (no live store): a scripted sender whose
    // handles stay `.sending` so the optimistic pending / spinner states are
    // observable, and a paused gate whose `-uitest-linkstate` forced state wins.
    @State private var scriptedSender = ScriptedChatCommandSender()
    @State private var fixtureSource = FixtureConnectionSource()
    #endif

    @Environment(\.dismiss) private var dismiss

    // Scroll-follow state (the pure decision lives in `ChatTranscript`).
    @State private var isNearBottom = true
    @State private var userScrolled = false
    @State private var didInitialScroll = false

    private let bottomAnchor = "chat-bottom-anchor"

    init(projectId: String,
         sessionId: String,
         store: TransportStore? = nil) {
        self.store = store
        _model = State(wrappedValue: ChatViewModel(
            projectId: Wire.ProjectId(projectId),
            sessionId: Wire.SessionId(sessionId)))
    }

    var body: some View {
        // Read the observable state here, in the non-lazy top scope of `body`,
        // so Observation reliably tracks it: the transcript's `LazyVStack` /
        // `ForEach` content closures are evaluated lazily and can fall outside
        // the tracked scope, which otherwise drops pill-expand and streamed-
        // append re-renders. Plain values are then threaded into the list.
        let rows = model.rows
        let expandedIDs = model.expandedItemIDs
        let pendingID = model.pendingPromptItemId
        let needsInput = model.isNeedsInput
        let canLoadEarlier = model.canLoadEarlier
        let sendStates = sendStateMap()
        let permissionActions = model.permissionActions
        let currentPending = model.currentPendingPromptId
        let commandsPaused = model.commandsPaused

        return VStack(spacing: 0) {
            header
            ChatSurfaceSwitcher(surface: $model.surface)
                .padding(.horizontal, Theme.Spacing.lg)
                .padding(.bottom, Theme.Spacing.sm)

            // The `.shell` segment mounts the minimal terminal (PRD §5.4);
            // `.agent` keeps the cleaned transcript + compose bar.
            if model.surface == .shell {
                ShellView(sessionId: model.sessionId,
                          sessionName: model.sessionName,
                          store: store)
            } else {
                transcript(rows: rows, expandedIDs: expandedIDs, pendingID: pendingID,
                           needsInput: needsInput, canLoadEarlier: canLoadEarlier,
                           sendStates: sendStates, permissionActions: permissionActions,
                           currentPending: currentPending, commandsPaused: commandsPaused)

                ChatComposeBar(sessionName: model.sessionName,
                               text: $model.draft,
                               commandsPaused: commandsPaused,
                               onSend: { model.send() })
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep)
        .navigationBarBackButtonHidden(true)
        .task {
            // Fixture (DEBUG / preview) wins so UI tests are deterministic;
            // otherwise bind to the live store and stream deltas in.
            if model.loadFixtureIfRequested() {
                #if DEBUG
                model.configureSend(sender: scriptedSender,
                                    pausedGate: CommandsPausedGate(source: fixtureSource))
                #endif
                return
            }
            if let store { model.bind(to: store) }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("AgentChatView")
    }

    /// A snapshot of outgoing send states keyed by their local render id, read
    /// in `body`'s tracked scope so per-row lookups don't re-track lazily.
    private func sendStateMap() -> [Wire.ItemId: OutgoingState] {
        var map: [Wire.ItemId: OutgoingState] = [:]
        for msg in model.outgoing { map[msg.localId] = msg.state }
        return map
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: Theme.Spacing.md) {
            Button {
                dismiss()
            } label: {
                Image(systemName: "chevron.left")
                    .font(.system(size: 18, weight: .semibold))
                    .foregroundStyle(Theme.textPrimary)
                    .frame(width: 32, height: 32)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("chat-back")

            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(model.sessionName)
                    .typography(Typography.headline)
                    .foregroundStyle(Theme.textPrimary)
                    .lineLimit(1)
                HStack(spacing: Theme.Spacing.xs) {
                    StatusDot(status: model.status.designSystem)
                    Text(model.agentType.displayName)
                        .typography(Typography.caption)
                        .foregroundStyle(Theme.textMuted)
                }
            }
            Spacer(minLength: 0)
            SessionActionsButton(sessionId: model.sessionId, sessionName: model.sessionName, status: model.status, store: store) // Control feature hook (PRD §5.6)
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.vertical, Theme.Spacing.md)
        .background(Theme.bgRaised)
    }

    // MARK: - Transcript

    private func transcript(rows: [ChatRow], expandedIDs: Set<Wire.ItemId>,
                            pendingID: Wire.ItemId?, needsInput: Bool,
                            canLoadEarlier: Bool,
                            sendStates: [Wire.ItemId: OutgoingState],
                            permissionActions: [Wire.PromptId: PermissionActionState],
                            currentPending: Wire.PromptId?,
                            commandsPaused: Bool) -> some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: Theme.Spacing.md) {
                    if canLoadEarlier {
                        loadEarlierButton
                    }
                    ForEach(rows) { row in
                        let promptId = row.item.permissionPromptId
                        let actionState = promptId.map { permissionActions[$0] ?? .idle } ?? .idle
                        let actionable: Bool = {
                            guard let promptId, promptId == currentPending, !commandsPaused else { return false }
                            switch actionState {
                            case .idle, .failed: return true
                            case .sending, .resolved, .stale: return false
                            }
                        }()
                        TranscriptRowView(
                            row: row,
                            isExpanded: expandedIDs.contains(row.item.itemId),
                            isPending: row.item.itemId == pendingID && needsInput,
                            onToggle: {
                                withAnimation(.easeInOut(duration: 0.2)) {
                                    model.toggleExpanded(row.item.itemId)
                                }
                            },
                            sendState: sendStates[row.item.itemId],
                            permissionState: actionState,
                            permissionActionable: actionable,
                            onDecide: { pid, choice in
                                model.decidePermission(promptId: pid, choice: choice)
                            },
                            onRetryPermission: { model.retryPermission($0) },
                            onRetryOutgoing: { model.retryOutgoing($0) })
                        .id(row.id)
                    }
                    Color.clear
                        .frame(height: 1)
                        .id(bottomAnchor)
                }
                .padding(.horizontal, Theme.Spacing.lg)
                .padding(.vertical, Theme.Spacing.md)
            }
            .scrollDismissesKeyboard(.interactively)
            .onScrollGeometryChange(for: Bool.self) { geo in
                let distanceFromBottom = geo.contentSize.height
                    - (geo.contentOffset.y + geo.containerSize.height)
                return distanceFromBottom <= 80
            } action: { _, near in
                isNearBottom = near
                userScrolled = !near
            }
            .overlay(alignment: .bottomTrailing) {
                if !isNearBottom {
                    jumpToLatestButton(proxy: proxy)
                }
            }
            .onChange(of: rows.count) { _, _ in
                guard ChatTranscript.shouldAutoScroll(isNearBottom: isNearBottom,
                                                      userScrolled: userScrolled) else { return }
                withAnimation(.easeOut(duration: 0.2)) {
                    proxy.scrollTo(bottomAnchor, anchor: .bottom)
                }
            }
            .onChange(of: rows.count) { _, count in
                if count > 0 { performInitialScroll(proxy: proxy) }
            }
            .onAppear {
                performInitialScroll(proxy: proxy)
            }
        }
    }

    private var loadEarlierButton: some View {
        Button {
            model.loadEarlier()
        } label: {
            Text("Load earlier")
                .typography(Typography.callout)
                .foregroundStyle(Theme.accent)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.sm)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("chat-load-earlier")
    }

    private func jumpToLatestButton(proxy: ScrollViewProxy) -> some View {
        Button {
            userScrolled = false
            isNearBottom = true
            withAnimation(.easeOut(duration: 0.2)) {
                proxy.scrollTo(bottomAnchor, anchor: .bottom)
            }
        } label: {
            Image(systemName: "arrow.down")
                .font(.system(size: 16, weight: .bold))
                .foregroundStyle(Theme.bgDeep)
                .frame(width: 40, height: 40)
                .background(Circle().fill(Theme.accent))
                .shadow(color: Theme.accent.opacity(0.5), radius: 8)
        }
        .buttonStyle(.plain)
        .padding(Theme.Spacing.lg)
        .accessibilityIdentifier("chat-jump-to-latest")
    }

    /// On entry (once items exist) scroll to the pending permission prompt if
    /// there is one, otherwise to the newest item.
    private func performInitialScroll(proxy: ScrollViewProxy) {
        guard !didInitialScroll, !model.rows.isEmpty else { return }
        didInitialScroll = true
        if let pending = model.pendingPromptItemId {
            proxy.scrollTo(pending.rawValue, anchor: .center)
        } else {
            proxy.scrollTo(bottomAnchor, anchor: .bottom)
        }
    }
}

#if DEBUG
#Preview {
    // Seeds the fixture transcript automatically under previews (see
    // `ChatViewModel.loadFixtureIfRequested`).
    NavigationStack {
        AgentChatView(projectId: "p1", sessionId: "s1")
    }
    .preferredColorScheme(.dark)
}
#endif
