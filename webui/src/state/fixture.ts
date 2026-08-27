import type { ActivityEvent, Project, SeatInfo, Snapshot } from "./model";

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
      seat: "controlling",
      isDesktop: true,
      sinceLabel: "since launch",
    },
    {
      label: "this tab",
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
