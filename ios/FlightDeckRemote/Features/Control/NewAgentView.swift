//
//  NewAgentView.swift
//  FlightDeckRemote
//
//  The real New-Agent flow in the FAB's sheet slot (PRD §5.5): ONE screen —
//  pick the agent type (Claude Code / OpenCode / Codex CLI), name the session
//  (names the worktree + branch; a live `flightdeck/<slug>` preview mirrors
//  the desktop's slugify rules exactly — `BranchSlug`), choose the base
//  branch (defaults from the project's known base when the git status feed
//  has one, else `main`), dictate or type the first task (v1 mic = system
//  keyboard dictation, PRD §7) → Launch agent.
//
//  Model/effort inherit the desktop's defaults and are not editable here.
//
//  Sending is paused-gated (PRD §8) and honest (PRD §5.8): the CTA shows a
//  spinner while in flight; `accepted` means creation *started* on the
//  desktop (async) — the sheet shows "Launching <name>…" and dismisses (the
//  new session appears via the snapshot delta); a rejection shows the
//  desktop's exact reason verbatim, inline; a transport failure offers retry.
//

import SwiftUI

// MARK: - Form model (pure state + validation, unit-tested)

@MainActor
@Observable
final class NewAgentFormModel {
    var agentType: Wire.AgentType = .claudeCode
    var name: String = ""
    var baseBranch: String = "main"
    var firstTask: String = ""
    var selectedProjectId: Wire.ProjectId?

    /// The desktop's slug for the typed name (worktree + branch leaf).
    var slug: String { BranchSlug.slugify(name) }

    /// The live branch preview, e.g. `flightdeck/add-rate-limit` (nil until
    /// the name yields a non-empty slug).
    var branchPreview: String? {
        let s = slug
        guard !s.isEmpty else { return nil }
        return BranchSlug.branchName(prefix: BranchSlug.defaultPrefix, slug: s)
    }

    private var trimmedBase: String {
        baseBranch.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var trimmedTask: String {
        firstTask.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Launch is deliberate: a project, a sluggable name, a base branch and a
    /// first task are all required — and never while commands are paused.
    func isLaunchable(commandsPaused: Bool) -> Bool {
        !commandsPaused
            && selectedProjectId != nil
            && !slug.isEmpty
            && !trimmedBase.isEmpty
            && !trimmedTask.isEmpty
    }

    /// The `new_agent` command for the current form, or nil while incomplete.
    /// `name` on the wire is the SLUG (session name == worktree == branch
    /// leaf, PRD §5.5).
    func commandBody() -> Wire.CommandBody? {
        guard let projectId = selectedProjectId, !slug.isEmpty,
              !trimmedBase.isEmpty, !trimmedTask.isEmpty else { return nil }
        return .newAgent(projectId: projectId, agentType: agentType,
                         name: slug, baseBranch: trimmedBase,
                         firstTask: trimmedTask)
    }

    /// Seed defaults from the live snapshot: select the first project (when
    /// none picked yet) and default the base branch to the selected project's
    /// known base (from any session's git status detail), else keep `main`.
    func applyDefaults(snapshot: Wire.StateSnapshot?,
                       gitStatus: [Wire.SessionId: Wire.GitStatusDetail]) {
        if selectedProjectId == nil {
            selectedProjectId = snapshot?.projects.first?.projectId
        }
        guard let projectId = selectedProjectId,
              let project = snapshot?.projects.first(where: { $0.projectId == projectId })
        else { return }
        for session in project.sessions {
            if let base = gitStatus[session.sessionId]?.baseBranch, !base.isEmpty {
                baseBranch = base
                return
            }
        }
    }
}

// MARK: - Screen

struct NewAgentView: View {
    private let store: TransportStore?

    @Environment(\.dismiss) private var dismiss
    @State private var model = NewAgentFormModel()
    @State private var gate: CommandsPausedGate
    @State private var runner: CommandRunner
    @FocusState private var isTaskFocused: Bool

    init(store: TransportStore? = nil) {
        self.store = store
        let source: any ConnectionStatusSource = store ?? NewAgentFallbackConnectionSource()
        let gate = CommandsPausedGate(source: source)
        let sender: any ControlCommandSending
        if let store {
            sender = store
        } else {
            #if DEBUG
            sender = ScriptedControlCommandSender()
            #else
            sender = UnavailableControlCommandSender()
            #endif
        }
        _gate = State(initialValue: gate)
        _runner = State(initialValue: CommandRunner(sender: sender,
                                                    isPaused: { gate.commandsPaused }))
    }

    private var commandsPaused: Bool { gate.commandsPaused }

    private var projects: [Wire.ProjectState] {
        store?.snapshot?.projects ?? []
    }

    private var selectedProject: Wire.ProjectState? {
        projects.first { $0.projectId == model.selectedProjectId }
    }

    private var isInFlight: Bool { runner.phase == .inFlight }

    var body: some View {
        VStack(spacing: 0) {
            header
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.Spacing.lg) {
                    if commandsPaused {
                        Text("Commands are paused until the link is back. Nothing is sent blind.")
                            .typography(Typography.caption)
                            .foregroundStyle(Theme.textDim)
                            .accessibilityIdentifier("new-agent-paused-label")
                    }

                    if projects.count > 1 { projectPicker }

                    field(label: "Agent") { agentTypePicker }
                    field(label: "Session name") { nameField }
                    slugPreview
                    field(label: "Base branch") { baseField }
                    field(label: "First task") { taskField }

                    Text("Model & effort inherit your desktop defaults.")
                        .typography(Typography.caption)
                        .foregroundStyle(Theme.textDim)

                    outcomeRow
                    launchButton
                }
                .padding(Theme.Spacing.lg)
            }
            .scrollDismissesKeyboard(.interactively)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep)
        .presentationDragIndicator(.visible)
        .presentationBackground(Theme.bgDeep)
        .onAppear {
            model.applyDefaults(snapshot: store?.snapshot,
                                gitStatus: store?.gitStatus ?? [:])
        }
        .onChange(of: runner.phase) { _, phase in
            // `accepted`/`applied` = creation started on the desktop (async):
            // show "Launching…" briefly, then dismiss — the session arrives
            // via the snapshot delta.
            if case .succeeded = phase {
                Task { @MainActor in
                    try? await Task.sleep(for: .seconds(1.1))
                    dismiss()
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("NewAgentView")
    }

    // MARK: Header

    private var header: some View {
        HStack(spacing: Theme.Spacing.md) {
            Text("New agent session")
                .typography(Typography.title)
                .foregroundStyle(Theme.textPrimary)
            Spacer(minLength: Theme.Spacing.sm)
            Button("Close") { dismiss() }
                .typography(Typography.callout)
                .foregroundStyle(Theme.accent)
                .accessibilityIdentifier("new-agent-close")
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.top, Theme.Spacing.xl)
        .padding(.bottom, Theme.Spacing.sm)
    }

    // MARK: Fields

    private func field(label: String, @ViewBuilder content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            Text(label.uppercased())
                .typography(Typography.captionBold)
                .foregroundStyle(Theme.textDim)
            content()
        }
    }

    private var projectPicker: some View {
        field(label: "Project") {
            Menu {
                ForEach(projects, id: \.projectId) { project in
                    Button(project.name) {
                        model.selectedProjectId = project.projectId
                        model.applyDefaults(snapshot: store?.snapshot,
                                            gitStatus: store?.gitStatus ?? [:])
                    }
                }
            } label: {
                HStack {
                    Text(selectedProject?.name ?? "Choose a project")
                        .typography(Typography.body)
                        .foregroundStyle(Theme.textPrimary)
                    Spacer(minLength: Theme.Spacing.sm)
                    Image(systemName: "chevron.up.chevron.down")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Theme.textDim)
                }
                .padding(.horizontal, Theme.Spacing.md)
                .padding(.vertical, Theme.Spacing.sm)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                        .fill(Theme.bgField)
                )
            }
            .accessibilityIdentifier("new-agent-project-picker")
        }
    }

    private var agentTypePicker: some View {
        HStack(spacing: Theme.Spacing.sm) {
            ForEach([Wire.AgentType.claudeCode, .opencode, .codex], id: \.self) { type in
                let selected = model.agentType == type
                Button {
                    model.agentType = type
                } label: {
                    Text(type.displayName)
                        .typography(Typography.callout)
                        .foregroundStyle(selected ? Theme.bgDeep : Theme.textMuted)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, Theme.Spacing.sm)
                        .background(
                            RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                                .fill(selected ? Theme.accent : Theme.bgField)
                        )
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("new-agent-type-\(type.rawValue)")
            }
        }
    }

    private var nameField: some View {
        styledTextField(text: $model.name, placeholder: "add rate limit",
                        mono: false)
            .accessibilityIdentifier("new-agent-name-field")
    }

    private var slugPreview: some View {
        Text(model.branchPreview ?? "\(BranchSlug.defaultPrefix)…")
            .typography(Typography.monoSmall)
            .foregroundStyle(model.branchPreview == nil ? Theme.textDim : Theme.statusManual)
            .accessibilityIdentifier("new-agent-slug-preview")
    }

    private var baseField: some View {
        styledTextField(text: $model.baseBranch, placeholder: "main", mono: true)
            .accessibilityIdentifier("new-agent-base-field")
    }

    private var taskField: some View {
        HStack(alignment: .bottom, spacing: Theme.Spacing.sm) {
            TextField("", text: $model.firstTask, axis: .vertical)
                .lineLimit(3...8)
                .typography(Typography.body)
                .foregroundStyle(Theme.textPrimary)
                .tint(Theme.accent)
                .focused($isTaskFocused)
                .padding(.horizontal, Theme.Spacing.md)
                .padding(.vertical, Theme.Spacing.sm)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                        .fill(Theme.bgField)
                )
                .overlay(alignment: .topLeading) {
                    if model.firstTask.isEmpty {
                        Text("Dictate or type the first task…")
                            .typography(Typography.body)
                            .foregroundStyle(Theme.textDim)
                            .padding(.horizontal, Theme.Spacing.md)
                            .padding(.vertical, Theme.Spacing.sm)
                            .allowsHitTesting(false)
                            .accessibilityHidden(true)
                    }
                }
                .accessibilityIdentifier("new-agent-task-field")

            // v1 mic = system keyboard dictation (PRD §7): focus the field so
            // the keyboard (with its dictation key) comes up.
            Button {
                isTaskFocused = true
            } label: {
                Image(systemName: "mic.fill")
                    .font(.system(size: 18, weight: .semibold))
                    .foregroundStyle(Theme.textMuted)
                    .frame(width: 44, height: 44)
                    .background(Circle().fill(Theme.bgField))
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("new-agent-task-mic")
        }
    }

    private func styledTextField(text: Binding<String>, placeholder: String,
                                 mono: Bool) -> some View {
        TextField("", text: text)
            .typography(mono ? Typography.mono : Typography.body)
            .foregroundStyle(Theme.textPrimary)
            .tint(Theme.accent)
            .autocorrectionDisabled()
            .textInputAutocapitalization(.never)
            .padding(.horizontal, Theme.Spacing.md)
            .padding(.vertical, Theme.Spacing.sm)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                    .fill(Theme.bgField)
            )
            .overlay(alignment: .leading) {
                if text.wrappedValue.isEmpty {
                    Text(placeholder)
                        .typography(mono ? Typography.mono : Typography.body)
                        .foregroundStyle(Theme.textDim)
                        .padding(.horizontal, Theme.Spacing.md)
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                }
            }
    }

    // MARK: Outcome + CTA

    @ViewBuilder
    private var outcomeRow: some View {
        switch runner.phase {
        case .idle, .inFlight:
            EmptyView()
        case .succeeded:
            HStack(spacing: Theme.Spacing.sm) {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(Theme.statusIdle)
                Text("Launching \(model.slug)…")
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.textPrimary)
            }
            .accessibilityIdentifier("new-agent-launching")
        case let .rejected(reason):
            // The desktop's exact reason, verbatim.
            Text(reason)
                .typography(Typography.callout)
                .foregroundStyle(Theme.statusWorking)
                .accessibilityIdentifier("new-agent-rejected")
        case let .failed(reason, _):
            HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.sm) {
                Text("not delivered — \(reason)")
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.statusNeedsInput)
                Spacer(minLength: Theme.Spacing.sm)
                Button("Retry") { runner.retry() }
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.accent)
                    .disabled(commandsPaused)
                    .accessibilityIdentifier("new-agent-retry")
            }
            .accessibilityIdentifier("new-agent-failed")
        }
    }

    private var launchButton: some View {
        let enabled = model.isLaunchable(commandsPaused: commandsPaused) && !isInFlight
        return Button {
            if let body = model.commandBody() { runner.run(body) }
        } label: {
            HStack(spacing: Theme.Spacing.sm) {
                if isInFlight {
                    ProgressView().tint(Theme.bgDeep)
                }
                Text(isInFlight ? "Launching…" : "Launch agent")
                    .typography(Typography.bodyMedium)
                    .foregroundStyle(enabled || isInFlight ? Theme.bgDeep : Theme.textDim)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, Theme.Spacing.md)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                    .fill(enabled || isInFlight ? Theme.accent : Theme.bgField)
            )
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .accessibilityIdentifier("new-agent-launch")
    }
}

/// Permanently-down `ConnectionStatusSource` for store-less mounts (previews).
/// Under `-uitest-linkstate` the gate's DEBUG forced state wins regardless.
@MainActor
private final class NewAgentFallbackConnectionSource: ConnectionStatusSource {
    var linkState: RemoteLinkState = .disconnected
    var peerConnected: Bool?
}

#if DEBUG
#Preview {
    Color.black.sheet(isPresented: .constant(true)) {
        NewAgentView(store: {
            let store = TransportStoreFactory.makeDefault(arguments: [])
            store.debugSeed(snapshot: .uiTestFixture)
            return store
        }())
    }
    .preferredColorScheme(.dark)
}
#endif
