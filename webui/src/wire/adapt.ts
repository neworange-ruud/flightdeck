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
  CommandTarget,
  GitBarInfo,
  HostCommand,
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
  WireCommandView,
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

/**
 * One seat row, from whichever frame delivered it.
 *
 * **Exported so there is exactly one of these.** A seat list reaches the browser
 * two ways — inside a `Snapshot` and inside a `Delta::Seats` — and artboard 2f
 * draws the same three facts either way. Two mapping functions is how one path
 * came to quietly drop the `connected` row while the other kept it, so both
 * paths call this and the panel cannot depend on how the news arrived.
 *
 * `serverTimeMs` is the **host's** clock, sent beside the rows in both frames.
 * `null` means the host sent none (one from before `Delta::Seats` carried it),
 * and the row is then undated rather than dated against `Date.now()` — a local
 * clock cannot honestly measure a host instant, and a wrong one would print a
 * confident wrong duration.
 */
export function seatOf(
  seat: WireSeatInfo,
  serverTimeMs: number | null,
): SeatInfo {
  return {
    label: seat.label,
    /**
     * Both facts come from their own wire field, and neither is ever recovered
     * from `label` by splitting it — see `WireSeatInfo`. A host from before the
     * split sends neither, and `null` then means "unknown", which is what 2f
     * renders as a missing row rather than as an empty one.
     */
    address: seat.address ?? null,
    browser: seat.user_agent_label ?? null,
    seat: seat.seat,
    /**
     * The turn, kept apart from the role for the same reason the host keeps
     * them apart: several rows may be `writing`, and at most one of them is
     * typing. Absent is `false` — not "the lock is free", but "this row is not
     * the one holding it", which is true of every row a host that omits it
     * sends.
     */
    holdsInput: seat.holds_input ?? false,
    isDesktop: seat.viewer_id === null,
    /**
     * The host's answer, not ours. It builds a frame per recipient precisely so
     * it can mark this row; a browser that instead matched the row's `address`
     * against its own would be right until two tabs share a machine, which is
     * the case the multi-viewer panel exists for.
     */
    isYou: seat.is_you === true,
    sinceLabel:
      serverTimeMs === null ? "" : agoLabel(serverTimeMs - seat.since_ms),
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

/** The four target kinds this build knows how to fill an id into. Anything
 * else is the host's `#[serde(other)]` arm reaching us — kept as
 * `unrecognized` rather than dropped to `null`, because a template row with a
 * target we cannot fill is skipped, not sent with no argument. */
const COMMAND_TARGETS: readonly CommandTarget[] = [
  "project",
  "session",
  "terminal",
  "unread_activity",
];

function targetOf(target: string | null | undefined): CommandTarget | null {
  if (target === null || target === undefined) {
    return null;
  }
  return COMMAND_TARGETS.find((known) => known === target) ?? "unrecognized";
}

/**
 * One inventory row → the palette's model (`remote-control-ll5.12`).
 *
 * Pure rename plus the absent-is-`null` convention every other adapter here
 * uses. Nothing is defaulted into existence: a row the host sent without an
 * annotation has none, and a row it sent without a refusal is one it runs.
 */
export function commandOf(wire: WireCommandView): HostCommand {
  return {
    id: wire.id,
    label: wire.label,
    group: wire.group,
    run: wire.run,
    hostOnly: wire.host_only ?? false,
    answersDialog: wire.answers_dialog ?? false,
    annotation: wire.annotation ?? null,
    target: targetOf(wire.target),
    refusal: wire.refusal ?? null,
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
    /** `remote-control-ll5.12`: the palette is exactly what the host lists
     * here. A host that sends none offers none — there is deliberately no
     * locally-authored inventory to fall back to. */
    commands: (wire.commands ?? []).map(commandOf),
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
    /** Absent means the host named no second step, which is the common case and
     * the honest reading of silence: a gate the browser invented would ask for a
     * name the host is not going to check. */
    gate:
      body.confirm_gate === undefined || body.confirm_gate === null
        ? null
        : {
            key: body.confirm_gate.key,
            expected: body.confirm_gate.expected,
            instruction: body.confirm_gate.instruction,
          },
    draft: { text: "", index: null, toggled: false, confirmName: "", step: 1 },
    pending: [],
    lastOutcome: null,
  };
}
