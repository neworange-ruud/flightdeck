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

// MARK: - Aggregated project option (one project on one machine)

/// One selectable project in the New-Agent picker, tagged with the machine
/// (pairing) it lives on so the launch command routes to that machine's store
/// (remote-control-cyj). Aggregating across every paired machine mirrors the
/// Projects tab (remote-control-aj2), which otherwise silently hid every
/// project on all but one machine. `pairingId == nil` / `machineName == nil` in
/// the single-store fallback (no coordinator handles — previews, UI-test
/// fixtures, an unpaired device), where there is only ever one machine to pick.
struct NewAgentProjectOption: Identifiable {
    /// The pairing this project belongs to (`nil` in the single-store fallback).
    let pairingId: String?
    /// The resolved machine chip label (override > desktop-reported > fallback),
    /// or `nil` when there is only one machine and no indicator is needed.
    let machineName: String?
    let project: Wire.ProjectState
    /// The transport to send this project's `new_agent` command through.
    let store: TransportStore
    /// The base branch to default to for this project (its machine's git status,
    /// else `main`).
    let defaultBaseBranch: String

    /// Stable across machines even when two machines share a project id.
    var id: String { (pairingId ?? "-") + "\u{1f}" + project.projectId.rawValue }
}

// MARK: - Form model (pure state + validation, unit-tested)

@MainActor
@Observable
final class NewAgentFormModel {
    var agentType: Wire.AgentType = .claudeCode
    var name: String = ""
    var baseBranch: String = "main"
    var firstTask: String = ""
    var selectedProjectId: Wire.ProjectId?
    /// The pairing of the selected project (remote-control-cyj): projects are
    /// aggregated across machines, so a project id alone no longer identifies a
    /// selection — the machine it lives on picks the store the launch routes to.
    /// `nil` in the single-store fallback (only one machine to launch on).
    var selectedPairingId: String?

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
    ///
    /// Single-store path (previews, UI-test fixtures, unpaired device). The
    /// multi-machine path is `applyDefaults(options:)`.
    func applyDefaults(snapshot: Wire.StateSnapshot?,
                       gitStatus: [Wire.SessionId: Wire.GitStatusDetail]) {
        if selectedProjectId == nil {
            selectedProjectId = snapshot?.projects.first?.projectId
        }
        guard let projectId = selectedProjectId,
              let project = snapshot?.projects.first(where: { $0.projectId == projectId })
        else { return }
        baseBranch = Self.defaultBaseBranch(project: project, gitStatus: gitStatus)
    }

    /// Seed defaults across projects aggregated from every paired machine
    /// (remote-control-cyj): select the first option (project+machine) when none
    /// is picked yet, then adopt the selected option's known base branch. Keeps
    /// an explicit selection (matched by BOTH project id and pairing, since a
    /// project id can repeat across machines).
    func applyDefaults(options: [NewAgentProjectOption]) {
        if selectedProjectId == nil, let first = options.first {
            selectedProjectId = first.project.projectId
            selectedPairingId = first.pairingId
        }
        guard let option = options.first(where: {
            $0.project.projectId == selectedProjectId && $0.pairingId == selectedPairingId
        }) else { return }
        baseBranch = option.defaultBaseBranch
    }

    /// The base branch to default to for `project`: the first non-empty
    /// `baseBranch` across its sessions' git status details, else `main`.
    static func defaultBaseBranch(
        project: Wire.ProjectState,
        gitStatus: [Wire.SessionId: Wire.GitStatusDetail]
    ) -> String {
        for session in project.sessions {
            if let base = gitStatus[session.sessionId]?.baseBranch, !base.isEmpty {
                return base
            }
        }
        return "main"
    }
}

// MARK: - Screen

struct NewAgentView: View {
    private let store: TransportStore?
    /// When set with live handles, the picker AGGREGATES projects across every
    /// paired machine (remote-control-cyj) and the launch routes to the selected
    /// project's own machine — otherwise New-Agent could only ever create on one
    /// machine, silently hiding every project on all the others. `nil` in
    /// previews/tests keeps the simple single-`store` behaviour.
    private let coordinator: TransportCoordinator?
    /// Resolves each machine's display name (override > desktop > fallback) for
    /// the per-project machine indicator; `nil` falls back to each handle's own
    /// `instance.displayName`.
    private let pairingStore: PairingStore?

    @Environment(\.dismiss) private var dismiss
    @State private var model: NewAgentFormModel
    @State private var gate: CommandsPausedGate
    @State private var runner: CommandRunner
    @FocusState private var isTaskFocused: Bool

    init(store: TransportStore? = nil,
         coordinator: TransportCoordinator? = nil,
         pairingStore: PairingStore? = nil) {
        self.store = store
        self.coordinator = coordinator
        self.pairingStore = pairingStore

        let model = NewAgentFormModel()
        _model = State(initialValue: model)

        // Resolve the transport for the currently-selected project's machine.
        // Aggregating (live handles) → the selected pairing's store, else the
        // live primary; otherwise the single `store`. Reads observable state
        // (`handles`, `selectedPairingId`, each store's `linkState`), so the
        // gate/runner below re-evaluate as the selection or a link changes.
        let resolveStore: @MainActor () -> TransportStore? = {
            if let coordinator, !coordinator.handles.isEmpty {
                if let pairingId = model.selectedPairingId,
                   let selected = coordinator.store(for: pairingId) {
                    return selected
                }
                return coordinator.primaryStore
            }
            return store
        }

        let source: any ConnectionStatusSource
        let sender: any ControlCommandSending
        if coordinator != nil || store != nil {
            source = ResolvingConnectionSource(
                resolve: resolveStore,
                fallback: store ?? NewAgentFallbackConnectionSource())
            sender = ResolvingControlCommandSender(
                resolve: resolveStore,
                fallback: store ?? UnavailableControlCommandSender())
        } else {
            source = NewAgentFallbackConnectionSource()
            #if DEBUG
            sender = ScriptedControlCommandSender()
            #else
            sender = UnavailableControlCommandSender()
            #endif
        }
        let gate = CommandsPausedGate(source: source)
        _gate = State(initialValue: gate)
        _runner = State(initialValue: CommandRunner(sender: sender,
                                                    isPaused: { gate.commandsPaused }))
    }

    private var commandsPaused: Bool { gate.commandsPaused }

    /// Every selectable project, aggregated across paired machines when a
    /// coordinator with live handles is present (remote-control-cyj), otherwise
    /// the single `store`'s projects. Each option carries the machine it belongs
    /// to so a launch routes to that machine's transport.
    private var projectOptions: [NewAgentProjectOption] {
        if let coordinator, !coordinator.handles.isEmpty {
            let multipleMachines = coordinator.handles.count > 1
            return coordinator.handles.flatMap { handle in
                (handle.store.snapshot?.projects ?? []).map { project in
                    NewAgentProjectOption(
                        pairingId: handle.pairingId,
                        machineName: multipleMachines ? machineName(for: handle) : nil,
                        project: project,
                        store: handle.store,
                        defaultBaseBranch: NewAgentFormModel.defaultBaseBranch(
                            project: project, gitStatus: handle.store.gitStatus))
                }
            }
        }
        guard let store else { return [] }
        return (store.snapshot?.projects ?? []).map { project in
            NewAgentProjectOption(
                pairingId: nil, machineName: nil, project: project, store: store,
                defaultBaseBranch: NewAgentFormModel.defaultBaseBranch(
                    project: project, gitStatus: store.gitStatus))
        }
    }

    private var selectedOption: NewAgentProjectOption? {
        projectOptions.first {
            $0.project.projectId == model.selectedProjectId
                && $0.pairingId == model.selectedPairingId
        }
    }

    /// The machine label for a handle: the live `PairingStore` name (override >
    /// desktop-reported > fallback) when available, else the handle's own
    /// instance name — matching the feed's chip resolution.
    private func machineName(for handle: TransportCoordinator.ClientHandle) -> String {
        if let pairingStore,
           let instance = pairingStore.instances.first(where: { $0.pairingId == handle.pairingId }) {
            return instance.displayName
        }
        return handle.instance.displayName
    }

    /// Re-seed defaults from the current source (aggregated when a coordinator
    /// with handles is present, else the single store).
    private func applyDefaults() {
        if let coordinator, !coordinator.handles.isEmpty {
            model.applyDefaults(options: projectOptions)
        } else {
            model.applyDefaults(snapshot: store?.snapshot,
                                gitStatus: store?.gitStatus ?? [:])
        }
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

                    if projectOptions.count > 1 { projectPicker }

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
        .onAppear { applyDefaults() }
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
                ForEach(projectOptions) { option in
                    Button {
                        model.selectedProjectId = option.project.projectId
                        model.selectedPairingId = option.pairingId
                        applyDefaults()
                    } label: {
                        // A machine-qualified label so two machines' same-named
                        // projects stay distinguishable in the menu (cyj).
                        if let machine = option.machineName {
                            Text("\(option.project.name) — \(machine)")
                        } else {
                            Text(option.project.name)
                        }
                    }
                }
            } label: {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(selectedOption?.project.name ?? "Choose a project")
                            .typography(Typography.body)
                            .foregroundStyle(Theme.textPrimary)
                        if let machine = selectedOption?.machineName {
                            Text(machine)
                                .typography(Typography.caption)
                                .foregroundStyle(Theme.textDim)
                                .accessibilityIdentifier("new-agent-project-machine")
                        }
                    }
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
            ForEach([Wire.AgentType.claudeCode, .opencode, .codex, .cursor], id: \.self) { type in
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

/// A `ConnectionStatusSource` that reads the link of whichever machine is
/// currently selected (remote-control-cyj): the commands-paused gate must
/// reflect the SELECTED project's machine, not a fixed one, so switching to an
/// offline machine pauses the launch honestly. Reads observable state on each
/// access, so the gate re-evaluates as the selection or a link changes; falls
/// back when no machine resolves yet.
@MainActor
private final class ResolvingConnectionSource: ConnectionStatusSource {
    private let resolve: @MainActor () -> TransportStore?
    private let fallback: any ConnectionStatusSource

    init(resolve: @escaping @MainActor () -> TransportStore?,
         fallback: any ConnectionStatusSource) {
        self.resolve = resolve
        self.fallback = fallback
    }

    var linkState: RemoteLinkState { (resolve() ?? fallback).linkState }
    var peerConnected: Bool? { (resolve() ?? fallback).peerConnected }
}

/// A `ControlCommandSending` that forwards each send to the SELECTED project's
/// machine (remote-control-cyj), so a `new_agent` is created on the machine the
/// chosen project lives on rather than always the primary one. Falls back when
/// no machine resolves (nothing paired / no selection yet).
@MainActor
private final class ResolvingControlCommandSender: ControlCommandSending {
    private let resolve: @MainActor () -> TransportStore?
    private let fallback: any ControlCommandSending

    init(resolve: @escaping @MainActor () -> TransportStore?,
         fallback: any ControlCommandSending) {
        self.resolve = resolve
        self.fallback = fallback
    }

    @discardableResult
    func sendControlCommand(_ body: Wire.CommandBody,
                            commandId: Wire.CommandId?) -> CommandHandle {
        (resolve() ?? fallback).sendControlCommand(body, commandId: commandId)
    }
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
