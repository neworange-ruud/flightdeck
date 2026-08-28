/**
 * Wire → model. The only place protocol v1's spelling becomes the app's.
 *
 * `remote-control-hgqy` left one decision open: protocol v1's `SessionView` is
 * flatter than `src/state/model.ts`'s `Session` in two places, and the note on
 * that issue was explicit that the browser must not *infer* either fact. Both
 * are mapped here from data the wire really carries, and each mapping says
 * which field it reads:
 *
 * - **The three-way git union.** `GitBar.has_upstream` and `GitBar.collected`
 *   are two bools that can encode a fourth, impossible state; `SessionGit` is a
 *   union of the three that exist. `collected: false` → `unknown` (git has not
 *   answered — `git: ?`, never `clean`), then `has_upstream: false` →
 *   `no_upstream`, else `known`. Both bools come from one `Option<&WorktreeStatus>`
 *   on the host, which is why the collapse is faithful rather than lossy.
 * - **`lifecycleNote`.** `SessionView::lifecycle_reporting` is the fact; the
 *   sentence §5.1 asks for (`unknown → unknown · Codex CLI reports no
 *   lifecycle`) is built from it plus `agent_display_name`. Absence of a status
 *   is never read as `idle`.
 *
 * Nothing here invents a value. Where v1 carries no answer the field is `null`
 * and the UI renders the absence.
 */

import type {
  ActivityEvent,
  GitBarInfo,
  Project,
  Seat,
  SeatInfo,
  Session,
  SessionGit,
  SessionStatus,
  Snapshot,
  TerminalTab,
} from "../state/model";
import type { DialogState } from "../state/types";
import type {
  WireActivityEvent,
  WireBucket,
  WireDialogView,
  WireGitBar,
  WireProjectView,
  WireSeatInfo,
  WireSessionStatus,
  WireSessionView,
  WireSnapshot,
  WireTerminalView,
} from "./frames";

/**
 * FlightDeck's interpreted labels (`status_keyword_to_interpreted`, mirrored in
 * `protocol.rs`) → the seven statuses the UI has a chip for.
 *
 * `bucket` is the fallback rather than the primary source because a bucket
 * cannot tell `starting` from `unknown`, and 1a renders those very differently.
 */
const STATUS_BY_LABEL: Readonly<Record<string, SessionStatus>> = {
  starting: "starting",
  running: "in_progress",
  working: "in_progress",
  "in progress": "in_progress",
  idle: "idle",
  completed: "idle",
  done: "idle",
  waiting: "waiting",
  "needs attention": "waiting",
  blocked: "waiting",
  failed: "error",
  "session lost": "error",
  recovered: "unknown",
  stopped: "unknown",
  unknown: "unknown",
};

const STATUS_BY_BUCKET: Readonly<Record<WireBucket, SessionStatus>> = {
  in_progress: "in_progress",
  idle: "idle",
  waiting: "waiting",
  error: "error",
  unknown: "unknown",
};

/**
 * Exported for `wire/socket.ts`'s live `Delta::Activity` handler, which needs
 * the exact same "unknown stays unknown" mapping `activityOf` below uses for
 * the snapshot backfill — see the bug this fixed: a live row was hardcoding
 * `"unknown"` regardless of what the host actually sent, so the same event
 * told two different stories depending on whether it arrived as backfill or
 * live.
 */
export function statusFromLabel(label: string, bucket: WireBucket): SessionStatus {
  return STATUS_BY_LABEL[label.toLowerCase()] ?? STATUS_BY_BUCKET[bucket];
}

/** The status to render, and the observed one it may be hiding. */
export function statusOf(status: WireSessionStatus): {
  status: SessionStatus;
  manual: boolean;
  observed: SessionStatus | null;
} {
  const observed = statusFromLabel(status.interpreted, status.bucket);
  if (status.manual === null) {
    return { status: observed, manual: false, observed: null };
  }
  /**
   * A hand-set status wins the chip, and the observed one is kept beside it —
   * 1a's `really: idle` row exists because the observed status is the one you
   * could act on wrongly.
   */
  return {
    status: statusFromLabel(status.manual, status.bucket),
    manual: true,
    observed,
  };
}

export function gitOf(git: WireGitBar, recovered: boolean): SessionGit {
  if (!git.collected) {
    return { kind: "unknown" };
  }
  if (!git.has_upstream) {
    return { kind: "no_upstream" };
  }
  return {
    kind: "known",
    dirty: git.added > 0 || git.modified > 0 || git.removed > 0,
    added: git.added,
    removed: git.removed,
    drift: git.drift,
    recovered,
  };
}

/**
 * The git bar's own row, or `null` when there is nothing to draw one from.
 *
 * A bar needs a branch name and a collected answer; without either the bar is
 * absent rather than half-drawn with zeros, which would read as `clean`.
 */
export function gitBarOf(git: WireGitBar, base: string): GitBarInfo | null {
  if (!git.collected || git.branch === null) {
    return null;
  }
  return {
    branch: git.branch,
    added: git.added,
    modified: git.modified,
    removed: git.removed,
    files: git.files_changed,
    ahead: git.ahead,
    behind: git.behind,
    baseAhead: git.drift,
    base,
  };
}

/**
 * The wire has three terminal roles, the tab strip has two kinds: the agent's
 * own terminal (`primary` on the wire, because it is the tab's first PTY) and a
 * `shell` opened beside it.
 */
function terminalOf(terminal: WireTerminalView): TerminalTab {
  return {
    id: terminal.terminal_id,
    title: terminal.title,
    kind: terminal.role === "shell" ? "shell" : "agent",
  };
}

export function sessionOf(session: WireSessionView, base: string): Session {
  const { status, manual, observed } = statusOf(session.status);
  return {
    id: session.session_id,
    name: session.name,
    agent: session.agent_display_name,
    status,
    manual,
    observed,
    /** The fact, not a guess: this agent exposes no lifecycle hooks. */
    lifecycleNote: session.lifecycle_reporting
      ? null
      : `${session.agent_display_name} reports no lifecycle`,
    /** No agent process exists yet, so there is no status to claim. */
    startingNote: session.phase === "creating" ? "creating worktree…" : null,
    git: gitOf(session.git, session.recovered ?? false),
    gitBar: gitBarOf(session.git, base),
    terminals: session.terminals.map(terminalOf),
  };
}

export function projectOf(project: WireProjectView): Project {
  return {
    id: project.project_id,
    name: project.name,
    sessions: project.sessions.map((s) => sessionOf(s, project.base_branch)),
  };
}

function seatOf(seat: WireSeatInfo, serverTimeMs: number): SeatInfo {
  return {
    label: seat.label,
    seat: seat.seat,
    isDesktop: seat.viewer_id === null,
    sinceLabel: agoLabel(serverTimeMs - seat.since_ms),
  };
}

/** `40s ago` / `4m ago` / `2h ago`, and `just now` under five seconds. */
export function agoLabel(millis: number): string {
  const seconds = Math.max(0, Math.round(millis / 1000));
  if (seconds < 5) {
    return "just now";
  }
  if (seconds < 60) {
    return `${seconds}s ago`;
  }
  if (seconds < 3600) {
    return `${Math.round(seconds / 60)}m ago`;
  }
  return `${Math.round(seconds / 3600)}h ago`;
}

function activityOf(
  event: WireActivityEvent,
  serverTimeMs: number,
): ActivityEvent {
  return {
    id: event.event_id,
    atLabel: agoLabel(serverTimeMs - event.at_ms),
    projectId: event.project_id,
    projectName: event.project_name,
    sessionId: event.session_id,
    sessionName: event.session_name,
    from: statusFromLabel(event.from, "unknown"),
    to: statusFromLabel(event.to, "unknown"),
    reason: event.reason ?? "",
    tier: event.tier,
    read: event.read ?? false,
  };
}

/**
 * A host snapshot as the store's `snapshot/received` wants it.
 *
 * `selection` is not optional in the model — the app always has something
 * selected — so a host that reports no selection (nothing open) falls back to
 * the first session it does report, and to empty strings only when there is
 * genuinely nothing at all.
 */
export function snapshotFromWire(wire: WireSnapshot): Snapshot {
  const projects = wire.projects.map(projectOf);
  const firstProject = projects[0] ?? null;
  const firstSession = firstProject?.sessions[0] ?? null;
  const selection = {
    projectId: wire.selection.project_id ?? firstProject?.id ?? "",
    sessionId: wire.selection.session_id ?? firstSession?.id ?? "",
    terminalId:
      wire.selection.terminal_id ?? firstSession?.terminals[0]?.id ?? "",
  };
  return {
    projects,
    selection,
    /** `remote-control-ll5.7`: `Selection::split_view` is a plain `bool` on
     * the wire (never absent), but the field is typed optional here because
     * `WireSelection` itself is a partial shape wherever a host predates the
     * flag — `?? false` is the same "never invent a fact" fallback the
     * project/session/terminal ids above already use, just for a bool. */
    splitView: wire.selection.split_view ?? false,
    geometry: { cols: wire.geometry.cols, rows: wire.geometry.rows },
    /** The chip counts seats, and the desktop is one of them. */
    viewers: wire.seats.length,
    latencyMs: null,
    /** The update chip is the *updater's* business, not the protocol's. */
    update: null,
    seats: wire.seats.map((s) => seatOf(s, wire.server_time_ms)),
    seat: wire.seat as Seat,
    activity: wire.activity.map((e) => activityOf(e, wire.server_time_ms)),
    /** D13: `null` is the host saying no dialog is open. A tab that attaches
     * while one is open paints it from here, because it never saw the
     * `Delta::DialogOpened` that announced it. */
    dialog:
      wire.dialog === undefined || wire.dialog === null
        ? null
        : dialogOf(wire.dialog),
  };
}

/**
 * `DialogView` → `DialogState` (D13).
 *
 * The draft starts empty and `index: null`, which means "the host's own
 * highlight stands" — the browser does not copy the host's selection into a
 * local field it would then have to keep in sync. Every field below comes off
 * the wire; nothing is invented, and a body the host omitted becomes an empty
 * shell rather than a guessed form.
 */
export function dialogOf(wire: WireDialogView): DialogState {
  const body = wire.body ?? {};
  return {
    id: wire.dialog_id,
    kind: wire.kind,
    title: wire.title,
    origin:
      wire.origin.origin === "browser"
        ? { kind: "browser", label: wire.origin.label }
        : { kind: "desktop" },
    input: body.input ?? null,
    list: (body.list ?? []).map((choice) => ({
      label: choice.label,
      selected: choice.selected,
    })),
    buttons: (body.buttons ?? []).map((button) => ({
      key: button.key,
      label: button.label,
    })),
    /** Absent means "the host did not say", and the honest reading of that is
     * *not* confirmable: a browser that guessed `true` would send a confirm the
     * host refuses, which is a worse experience than a disabled button. */
    confirmable: body.confirmable ?? false,
    refusal: body.refusal ?? null,
    draft: { text: "", index: null, toggled: false },
    pending: [],
    lastOutcome: null,
  };
}
