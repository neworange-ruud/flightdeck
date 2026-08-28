import type {
  ActivityEvent,
  HostCommand,
  Project,
  SeatInfo,
  Snapshot,
} from "./model";

/**
 * The fixture snapshot the main screen renders against today.
 *
 * **Why a fixture and not a socket.** Wiring the live websocket is
 * `remote-control-hgqy`; this file exists so the seven regions of artboard 1a
 * can be built, reviewed and *tested* before the wire exists. It is typed as
 * `Snapshot` — the exact payload `snapshot/received` carries — so hgqy's job
 * is to construct a `Snapshot` from `ServerMsg::Snapshot` and dispatch the
 * same action. No component changes when the fixture goes away.
 *
 * **Fidelity is the point.** Every project name, session name, agent name,
 * status, git number and terminal title below is copied from
 * `specs/design/flightdeck-web-turn2.dc.html` artboard `1a — MAIN, TERMINAL
 * MODE` (lines 806–1011), so the render can be compared against the artboard
 * by eye and asserted by test. Do not "tidy" these strings.
 *
 * One deliberate addition: 1a's sidebar shows the six `flightdeck` sessions
 * and nothing else, but turn 2 §5.1 requires the "unknown stays unknown" case
 * to exist. Rather than corrupt 1a's six rows (and its `6 sessions` footer
 * count), the no-lifecycle agent lives in the **`api-gateway`** project, which
 * 1a does not draw. Selecting that project renders it — which is also how the
 * shared-selection path (D3) gets exercised.
 */

const flightdeck: Project = {
  id: "p-flightdeck",
  name: "flightdeck",
  sessions: [
    {
      id: "s-fix-login-redirect",
      name: "fix-login-redirect",
      agent: "Claude Code",
      status: "in_progress",
      manual: false,
      observed: null,
      lifecycleNote: null,
      startingNote: null,
      git: {
        kind: "known",
        dirty: true,
        added: 3,
        removed: 0,
        drift: 3,
        recovered: false,
      },
      gitBar: {
        branch: "flightdeck/fix-login-redirect",
        added: 3,
        modified: 2,
        removed: 1,
        files: 6,
        ahead: 3,
        behind: 0,
        baseAhead: 4,
        base: "main",
      },
      terminals: [
        { id: "t-agent", title: "agent", kind: "agent" },
        { id: "t-shell-1", title: "shell 1", kind: "shell" },
        { id: "t-shell-2", title: "shell 2", kind: "shell" },
      ],
    },
    {
      id: "s-add-tests-api",
      name: "add-tests-api",
      agent: "OpenCode",
      status: "idle",
      manual: false,
      observed: null,
      lifecycleNote: null,
      startingNote: null,
      git: { kind: "no_upstream" },
      gitBar: {
        branch: "flightdeck/add-tests-api",
        added: 0,
        modified: 0,
        removed: 0,
        files: 0,
        ahead: 0,
        behind: 0,
        baseAhead: 0,
        base: "main",
      },
      terminals: [{ id: "t-add-tests-agent", title: "agent", kind: "agent" }],
    },
    {
      id: "s-migrate-schema-v4",
      name: "migrate-schema-v4",
      agent: "Codex CLI",
      status: "waiting",
      manual: false,
      observed: null,
      lifecycleNote: null,
      startingNote: null,
      git: {
        kind: "known",
        dirty: true,
        added: 1,
        removed: 2,
        drift: null,
        recovered: false,
      },
      gitBar: {
        branch: "flightdeck/migrate-schema-v4",
        added: 1,
        modified: 1,
        removed: 2,
        files: 3,
        ahead: 0,
        behind: 0,
        baseAhead: 4,
        base: "main",
      },
      terminals: [{ id: "t-migrate-agent", title: "agent", kind: "agent" }],
    },
    {
      id: "s-flaky-e2e-runner",
      name: "flaky-e2e-runner",
      agent: "Claude Code",
      status: "error",
      manual: false,
      observed: null,
      lifecycleNote: null,
      startingNote: null,
      git: {
        kind: "known",
        dirty: false,
        added: 0,
        removed: 0,
        drift: 7,
        recovered: true,
      },
      gitBar: {
        branch: "flightdeck/flaky-e2e-runner",
        added: 0,
        modified: 0,
        removed: 0,
        files: 0,
        ahead: 0,
        behind: 2,
        baseAhead: 7,
        base: "main",
      },
      terminals: [{ id: "t-flaky-agent", title: "agent", kind: "agent" }],
    },
    {
      id: "s-perf-audit-images",
      name: "perf-audit-images",
      agent: "Claude Code",
      /** 1a: `[reviewing] ·set` — a human set this, and the observed status
       * underneath it is still `idle`. Both facts are rendered. */
      status: "reviewing",
      manual: true,
      observed: "idle",
      lifecycleNote: null,
      startingNote: null,
      git: {
        kind: "known",
        dirty: false,
        added: 0,
        removed: 0,
        drift: null,
        recovered: false,
      },
      gitBar: {
        branch: "flightdeck/perf-audit-images",
        added: 0,
        modified: 0,
        removed: 0,
        files: 0,
        ahead: 0,
        behind: 0,
        baseAhead: 4,
        base: "main",
      },
      terminals: [{ id: "t-perf-agent", title: "agent", kind: "agent" }],
    },
    {
      id: "s-hotfix-csp-header",
      name: "hotfix-csp-header",
      agent: "Claude Code",
      status: "starting",
      manual: false,
      observed: null,
      lifecycleNote: null,
      /** 1a renders this italic prose *instead of* a status chip — the session
       * has no agent process yet, so claiming one would be a guess. */
      startingNote: "creating worktree…",
      /** git has not answered yet: `git: ?`, a fact, at --fd-text-quiet. */
      git: { kind: "unknown" },
      gitBar: null,
      terminals: [{ id: "t-hotfix-agent", title: "agent", kind: "agent" }],
    },
  ],
};

const apiGateway: Project = {
  id: "p-api-gateway",
  name: "api-gateway",
  sessions: [
    {
      id: "s-sync-openapi-types",
      name: "sync-openapi-types",
      agent: "Codex CLI",
      /**
       * §5.1 "unknown stays unknown". This agent exposes no lifecycle hooks,
       * so the app reports the absence rather than inferring `idle` from
       * silence. The note is rendered verbatim next to the label, giving
       * `unknown → unknown · Codex CLI reports no lifecycle`.
       */
      status: "unknown",
      manual: false,
      observed: null,
      lifecycleNote: "Codex CLI reports no lifecycle",
      startingNote: null,
      git: { kind: "no_upstream" },
      gitBar: {
        branch: "api-gateway/sync-openapi-types",
        added: 0,
        modified: 4,
        removed: 0,
        files: 4,
        ahead: 0,
        behind: 0,
        baseAhead: 0,
        base: "main",
      },
      terminals: [{ id: "t-sync-agent", title: "agent", kind: "agent" }],
    },
    {
      id: "s-rotate-jwt-secret",
      name: "rotate-jwt-secret",
      agent: "Claude Code",
      status: "waiting",
      manual: false,
      observed: null,
      lifecycleNote: null,
      startingNote: null,
      git: {
        kind: "known",
        dirty: true,
        added: 12,
        removed: 4,
        drift: null,
        recovered: false,
      },
      gitBar: {
        branch: "api-gateway/rotate-jwt-secret",
        added: 12,
        modified: 3,
        removed: 4,
        files: 9,
        ahead: 1,
        behind: 0,
        baseAhead: 0,
        base: "develop",
      },
      terminals: [
        { id: "t-rotate-agent", title: "agent", kind: "agent" },
        { id: "t-rotate-shell", title: "shell 1", kind: "shell" },
      ],
    },
  ],
};

const web: Project = {
  id: "p-web",
  name: "web",
  sessions: [
    {
      id: "s-bump-deps",
      name: "bump-deps",
      agent: "OpenCode",
      status: "idle",
      manual: false,
      observed: null,
      lifecycleNote: null,
      startingNote: null,
      git: { kind: "no_upstream" },
      gitBar: {
        branch: "web/bump-deps",
        added: 0,
        modified: 1,
        removed: 0,
        files: 1,
        ahead: 0,
        behind: 0,
        baseAhead: 0,
        base: "main",
      },
      terminals: [{ id: "t-bump-agent", title: "agent", kind: "agent" }],
    },
    {
      id: "s-alt-text-audit",
      name: "alt-text-audit",
      agent: "Claude Code",
      status: "idle",
      manual: false,
      observed: null,
      lifecycleNote: null,
      startingNote: null,
      git: {
        kind: "known",
        dirty: false,
        added: 0,
        removed: 0,
        drift: null,
        recovered: false,
      },
      gitBar: {
        branch: "web/alt-text-audit",
        added: 0,
        modified: 0,
        removed: 0,
        files: 0,
        ahead: 0,
        behind: 0,
        baseAhead: 0,
        base: "main",
      },
      terminals: [{ id: "t-alt-text-agent", title: "agent", kind: "agent" }],
    },
  ],
};

/** The snapshot artboard 1a draws. */
export function fixtureSnapshot(): Snapshot {
  return {
    projects: [flightdeck, apiGateway, web],
    selection: {
      projectId: "p-flightdeck",
      sessionId: "s-fix-login-redirect",
      terminalId: "t-agent",
    },
    /** 1a/1b are drawn single-pane; 1c is reached by dispatching
     * `layout/set` on top of this fixture (see `mainScreen.test.ts`), not by
     * this flag — a fixture that started split would draw 1c for every test
     * that loads it. */
    splitView: false,
    /** D4: 1a's chip reads `120×34 · host owns geometry`. */
    geometry: { cols: 120, rows: 34 },
    /** 1a drew a count; 2c/2f name the seats instead — see `fixtureSeats`. */
    viewers: 2,
    latencyMs: 18,
    update: { version: "v1.16.0" },
    /** 1a is a browser that holds the keyboard, so the host granted it the
     * controlling seat. An observer would see `MODE: —`, correctly. */
    seat: "controlling",
    seats: fixtureSeats(),
    activity: fixtureActivity(),
    /** D13: no dialog. 1a/1b/1c are the screen with nothing being asked; the
     * dialog artboards (1d/1e) are reached by dispatching `dialog/opened` on
     * top of this fixture, the same way 1c is reached with `layout/set`. */
    dialog: null,
    /** `remote-control-ll5.12`: the palette renders this and nothing else. */
    commands: fixtureCommands(),
  };
}

/**
 * 2c/2f's viewer chip: `desktop + this tab`.
 *
 * **Two named seats, not a counter that implies a crowd.** The desktop is
 * first because it is always there and is not a viewer at all (its
 * `SeatInfo::viewer_id` is `null` on the wire); this tab is second because it
 * is the one the reader is looking at.
 */
export function fixtureSeats(): readonly SeatInfo[] {
  return [
    {
      label: "desktop",
      /** No socket, so no address the host observed and no browser to name. */
      address: null,
      browser: null,
      seat: "controlling",
      isDesktop: true,
      sinceLabel: "since launch",
    },
    {
      label: "this tab",
      /** 2f's three facts, each in its own field. */
      address: "192.168.2.20",
      browser: "Chrome on macOS",
      seat: "controlling",
      isDesktop: false,
      sinceLabel: "14 minutes, active 20s ago",
    },
  ];
}

/**
 * The five rows artboard 2e draws, plus one cross-project row.
 *
 * **Copy is verbatim from 2e** — the transitions and the `reason` strings
 * (`asked a question`, `agent exited (code 1)`, `finished, 18 files touched`,
 * `set by hand on the desktop`, `Codex CLI reports no lifecycle`) are the part
 * a user actually reads, and they come from the host's `ActivityEvent.reason`
 * rather than being reconstructed from `from`/`to` — which could not produce
 * "18 files touched" at all.
 *
 * **Ids follow the fixture, not the artboard.** 2e attributes
 * `migrate-schema-v4` to `api-gateway` and `perf-audit-images` to `web`, while
 * artboard 1a puts both under `flightdeck`. A feed row whose ids named a
 * session that does not exist would make `jump` (D3) silently do nothing, which
 * is the one thing these rows must not do — so the ids point at the sessions 1a
 * really draws, and the last row deliberately lives in another project so the
 * cross-project jump is exercised.
 *
 * Oldest first, matching `Snapshot::activity`'s documented backfill order.
 */
export function fixtureActivity(): readonly ActivityEvent[] {
  return [
    {
      id: "e-1",
      atLabel: "31m ago",
      projectId: "p-flightdeck",
      projectName: "flightdeck",
      sessionId: "s-hotfix-csp-header",
      sessionName: "hotfix-csp-header",
      /** §5.1: unknown stays unknown, on both ends of the arrow. */
      from: "unknown",
      to: "unknown",
      reason: "Codex CLI reports no lifecycle",
      tier: "quiet",
      read: true,
    },
    {
      id: "e-2",
      atLabel: "26m ago",
      projectId: "p-flightdeck",
      projectName: "flightdeck",
      sessionId: "s-perf-audit-images",
      sessionName: "perf-audit-images",
      from: "idle",
      to: "reviewing",
      reason: "set by hand on the desktop",
      tier: "quiet",
      read: true,
    },
    {
      id: "e-3",
      atLabel: "11m ago",
      projectId: "p-flightdeck",
      projectName: "flightdeck",
      sessionId: "s-add-tests-api",
      sessionName: "add-tests-api",
      from: "in_progress",
      to: "idle",
      reason: "finished, 18 files touched",
      tier: "finished",
      read: false,
    },
    {
      id: "e-4",
      atLabel: "4m ago",
      projectId: "p-flightdeck",
      projectName: "flightdeck",
      sessionId: "s-flaky-e2e-runner",
      sessionName: "flaky-e2e-runner",
      from: "in_progress",
      to: "error",
      reason: "agent exited (code 1)",
      tier: "attention",
      read: false,
    },
    {
      id: "e-5",
      atLabel: "40s ago",
      projectId: "p-api-gateway",
      projectName: "api-gateway",
      sessionId: "s-rotate-jwt-secret",
      sessionName: "rotate-jwt-secret",
      from: "in_progress",
      to: "waiting",
      reason: "asked a question",
      tier: "attention",
      read: false,
    },
  ];
}

/**
 * The host's command inventory, exactly as `Snapshot::commands` delivers it.
 *
 * **This is a transcript, not an authored list.** Every row below was dumped
 * from `src/web/commands.rs`'s `INVENTORY` through `CommandSpec::view()` and
 * renamed the one way `wire/adapt.ts`'s `commandOf` renames it — labels,
 * groups, annotations, `hostOnly` flags and refusal sentences included, down to
 * the punctuation. Nothing here is the browser's opinion of what the host can
 * run; that is the whole point of `remote-control-ll5.12`, and a row that
 * disagreed with the host would be the exact bug the host-sent inventory
 * exists to prevent.
 *
 * Re-dump it rather than hand-editing it when the host's table changes.
 */
export function fixtureCommands(): readonly HostCommand[] {
  return [
    {
      id: "select_session",
      label: "Select Session",
      group: "Sessions",
      run: { name: "select_session" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: "session",
      refusal: null,
    },
    {
      id: "select_project",
      label: "Switch to Project",
      group: "Projects",
      run: { name: "select_project" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: "project",
      refusal: null,
    },
    {
      id: "select_terminal",
      label: "Select Terminal",
      group: "Terminals",
      run: { name: "select_terminal" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: "terminal",
      refusal: null,
    },
    {
      id: "open_project",
      label: "Open Project",
      group: "Projects",
      run: { name: "open_project" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "close_project",
      label: "Close Project",
      group: "Projects",
      run: { name: "close_project" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "next_project",
      label: "Next Project",
      group: "Projects",
      run: { name: "next_project" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "previous_project",
      label: "Previous Project",
      group: "Projects",
      run: { name: "previous_project" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "new_agent_session_tab",
      label: "New Agent Session Tab",
      group: "Agent Session Tabs",
      run: { name: "new_agent_session_tab" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "rename_agent_session_tab",
      label: "Rename Agent Session Tab",
      group: "Agent Session Tabs",
      run: { name: "rename_agent_session_tab" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "close_agent_session_tab",
      label: "Close Agent Session Tab",
      group: "Agent Session Tabs",
      run: { name: "close_agent_session_tab" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "switch_agent_session_tab",
      label: "Switch Agent Session Tab",
      group: "Agent Session Tabs",
      run: { name: "switch_agent_session_tab" },
      hostOnly: false,
      answersDialog: false,
      annotation: "next",
      target: null,
      refusal: null,
    },
    {
      id: "restart_agent",
      label: "Restart Agent",
      group: "Agent Session Tabs",
      run: { name: "restart_agent" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "rebase_worktree",
      label: "Rebase Worktree",
      group: "Worktree",
      run: { name: "rebase_worktree" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "abandon_worktree",
      label: "Abandon Worktree",
      group: "Worktree",
      run: { name: "abandon_worktree" },
      hostOnly: false,
      answersDialog: false,
      annotation: "destructive",
      target: null,
      /** It runs from a browser since `remote-control-ll5.4`: the row opens
       * SPECS §5/§15's shared question, and artboard 1g's typed-name step
       * stands in front of the answer rather than in front of the row. */
      refusal: null,
    },
    {
      id: "open_worktree_in_file_manager",
      label: "Open Worktree in File Manager",
      group: "Worktree",
      run: { name: "open_worktree_in_file_manager" },
      hostOnly: true,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal:
        "This opens a window on the machine running FlightDeck, which is not the machine this browser is on. Run it from the desktop.",
    },
    {
      id: "edit_in_editor",
      label: "Edit in $EDITOR",
      group: "Worktree",
      run: { name: "edit_in_editor" },
      hostOnly: true,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal:
        "This opens a window on the machine running FlightDeck, which is not the machine this browser is on. Run it from the desktop.",
    },
    {
      id: "push_branch",
      label: "Push Branch",
      group: "Git",
      run: { name: "push_branch" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "finish_local_merge",
      label: "Finish / Local Merge",
      group: "Git",
      run: { name: "finish_local_merge" },
      hostOnly: false,
      answersDialog: false,
      annotation: "destructive",
      target: null,
      refusal: null,
    },
    {
      id: "pull_base",
      label: "Pull Base",
      group: "Git",
      run: { name: "pull_base" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal:
        "Pull Base rebases your local base branch — and stashes, pulls over and re-applies any uncommitted work in the base folder — with no confirmation step to read first (SPECS §5.2). Rebase Worktree is offered here because §5.1 puts a shared confirmation in front of it; this one has none, so run it from the desktop (Ctrl-u).",
    },
    {
      id: "show_git_status",
      label: "Show Git Status",
      group: "Git",
      run: { name: "show_git_status" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal:
        "Git status opens a read-only overlay on the desktop, and the browser has no design for it yet (design turn 3). It is not one of D13's shared dialogs: nothing is being asked, so there is nothing to answer from here.",
    },
    {
      id: "new_child_terminal",
      label: "New Child Terminal",
      group: "Terminals",
      run: { name: "new_child_terminal" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "close_child_terminal",
      label: "Close Child Terminal",
      group: "Terminals",
      run: { name: "close_child_terminal" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "new_agent",
      label: "New Agent",
      group: "Terminals",
      run: { name: "new_agent" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "close_agent",
      label: "Close Agent",
      group: "Terminals",
      run: { name: "close_agent" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "switch_child_terminal",
      label: "Switch Child Terminal",
      group: "Terminals",
      run: { name: "switch_child_terminal" },
      hostOnly: false,
      answersDialog: false,
      annotation: "next",
      target: null,
      refusal: null,
    },
    {
      id: "open_shell",
      label: "Open Shell",
      group: "Terminals",
      run: { name: "open_shell" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "set_manual_status",
      label: "Set Manual Status",
      group: "Status",
      run: { name: "set_manual_status" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "open_configuration",
      label: "Open Configuration",
      group: "Configuration",
      run: { name: "open_configuration" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal:
        "The configuration manager is a browser surface of its own (remote-control-ll5.6); opening the desktop's overlay from here would put a modal on a screen this browser cannot see.",
    },
    {
      id: "pair_phone",
      label: "Pair Phone",
      group: "Remote",
      run: { name: "pair_phone" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal:
        "Pairing shows a QR code and a 4-digit code on the desktop's screen, which this build cannot render in a browser.",
    },
    {
      id: "unpair_phone",
      label: "Unpair Phone",
      group: "Remote",
      run: { name: "unpair_phone" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "start_web_interface",
      label: "Start Web Interface",
      group: "Remote",
      run: { name: "start_web_interface" },
      hostOnly: false,
      answersDialog: false,
      annotation: "already running",
      target: null,
      refusal:
        "The web interface is already running — this browser is connected to it.",
    },
    {
      id: "stop_web_interface",
      label: "Stop Web Interface",
      group: "Remote",
      run: { name: "stop_web_interface" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal:
        "Stopping the web interface would disconnect every browser, including this one. Like quit, that needs the two-step confirmation this build does not have yet — stop it from the desktop.",
    },
    {
      id: "toggle_split_view",
      label: "Toggle Split View",
      group: "View",
      run: { name: "toggle_split_view" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: null,
    },
    {
      id: "show_help",
      label: "Show Help",
      group: "View",
      run: { name: "show_help" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal:
        "The help overlay is a browser surface of its own (remote-control-ll5.8); this build would only open it on the desktop.",
    },
    {
      id: "about_flightdeck",
      label: "About FlightDeck",
      group: "View",
      run: { name: "about_flightdeck" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal:
        "The About dialog is a browser surface of its own (remote-control-ll5.8); this build would only open it on the desktop.",
    },
    {
      id: "quit",
      label: "Quit",
      group: "Global",
      run: { name: "quit" },
      hostOnly: false,
      answersDialog: false,
      annotation: "destructive",
      target: null,
      /** D16 said a `host only` badge is not enough for quit, and since
       * `remote-control-ll5.4` it is not a refusal either: the row opens the
       * shared confirmation, and artboard 1g's typed name guards the answer. */
      refusal: null,
    },
    {
      id: "request_snapshot",
      label: "Request Snapshot",
      group: "Session",
      run: { name: "request_snapshot" },
      hostOnly: false,
      answersDialog: false,
      annotation: "resync from host",
      target: null,
      refusal: null,
    },
    {
      id: "release_seat",
      label: "Release Seat",
      group: "Session",
      run: { name: "release_seat" },
      hostOnly: false,
      answersDialog: false,
      annotation: "give up control",
      target: null,
      refusal: null,
    },
    {
      id: "mark_activity_read",
      label: "Mark All Activity Read",
      group: "Session",
      run: { name: "mark_activity_read" },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: "unread_activity",
      refusal: null,
    },
    {
      id: "dialog_confirm",
      label: "Confirm Dialog",
      group: "Session",
      run: { name: "dialog_confirm" },
      hostOnly: false,
      answersDialog: true,
      annotation: "answers the open dialog",
      target: null,
      refusal: null,
    },
    {
      id: "dialog_cancel",
      label: "Cancel Dialog",
      group: "Session",
      run: { name: "dialog_cancel" },
      hostOnly: false,
      answersDialog: true,
      annotation: "answers the open dialog",
      target: null,
      refusal: null,
    },
  ];
}
