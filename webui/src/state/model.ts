/**
 * The domain model the main screen (artboards 1a/1b/1c) renders.
 *
 * This is deliberately a *view* model, not the wire protocol. `src/web/
 * protocol.rs` (D12) is being written by a concurrent task; when its
 * `ServerMsg::Snapshot` lands, `remote-control-hgqy` maps it onto `Snapshot`
 * below and dispatches `snapshot/received` — no component changes, because no
 * component reads anything but these types.
 *
 * Everything here is `readonly`: the reducer replaces, it never mutates.
 */

/**
 * The status vocabulary artboard 1a renders, plus turn 2 §5.1's `unknown`.
 *
 * `unknown` is not a synonym for "we haven't asked yet" — it is the credible
 * "we don't know", used for an agent with no lifecycle hooks. §5.1: *an agent
 * with no lifecycle hooks renders `○` and `unknown → unknown · Codex CLI
 * reports no lifecycle` rather than a guess.* Never map an absent status onto
 * `idle`; that is the guess the requirement exists to forbid.
 */
export type SessionStatus =
  | "in_progress"
  | "idle"
  | "waiting"
  | "error"
  | "reviewing"
  | "starting"
  | "unknown";

/**
 * Which palette token a status paints with. Named after the *meaning* 2g
 * documents, not after the colour, so a token rename is a one-line change in
 * `src/state/status.ts` and nothing else.
 */
export type StatusTone =
  | "accent" /* --fd-accent: in progress, reviewing */
  | "ok" /* --fd-ok: idle, healthy */
  | "alert" /* --fd-alert: waiting, error */
  | "quiet" /* --fd-text-quiet: unknown, starting — a fact, never decor */;

/** The glyph in front of a session name. `spinner` pulses (1a's `{{ spinner }}`). */
export type StatusGlyph = "dot" | "spinner" | "hollow";

/**
 * The sidebar's third line. A union rather than nullable numbers, because the
 * three "we cannot say" cases are different facts and 2g requires all three to
 * be legible: `no-upstream` and `git: ?` are **facts**, so they render at
 * `--fd-text-quiet`, never `--fd-text-decor`.
 */
export type SessionGit =
  | {
      readonly kind: "known";
      readonly dirty: boolean;
      readonly added: number;
      readonly removed: number;
      /** commits the worktree has drifted from base, `null` when not drifted */
      readonly drift: number | null;
      /** the session was recovered after a crash (1a: `[recovered]`) */
      readonly recovered: boolean;
    }
  /** the branch has no upstream — a fact about what a push would do */
  | { readonly kind: "no_upstream" }
  /** git has not answered yet (1a: `git: ?` while the worktree is created) */
  | { readonly kind: "unknown" };

/** The git info bar (1a, bottom strip) for the selected session. */
export interface GitBarInfo {
  readonly branch: string;
  readonly added: number;
  readonly modified: number;
  readonly removed: number;
  readonly files: number;
  readonly ahead: number;
  readonly behind: number;
  /** commits base has moved on by (1a: `base +4`) */
  readonly baseAhead: number;
  readonly base: string;
}

/** A terminal tab inside a session (1a: `agent`, `shell 1`, `shell 2`). */
export interface TerminalTab {
  readonly id: string;
  readonly title: string;
  /** `agent` paints with --fd-focus, `shell` with --fd-accent (2g) */
  readonly kind: "agent" | "shell";
}

export interface Session {
  readonly id: string;
  readonly name: string;
  /** the agent's product name — a fact, so --fd-text-quiet (2g) */
  readonly agent: string;
  readonly status: SessionStatus;
  /** a human set this status by hand (1a: `·set`) */
  readonly manual: boolean;
  /** the observed status under a manual override (1a: `really: idle`) */
  readonly observed: SessionStatus | null;
  /**
   * §5.1 "unknown stays unknown". Present only when the agent reports no
   * lifecycle at all; rendered verbatim so the app never invents a status.
   */
  readonly lifecycleNote: string | null;
  /** italic prose that replaces the status chip while starting up */
  readonly startingNote: string | null;
  readonly git: SessionGit;
  /** `null` when git has not answered for this session yet */
  readonly gitBar: GitBarInfo | null;
  readonly terminals: readonly TerminalTab[];
}

export interface Project {
  readonly id: string;
  readonly name: string;
  readonly sessions: readonly Session[];
}

/**
 * D3: **one** selected project / session / terminal for the whole instance.
 * This is not browser-local state — clicking a session here moves the
 * desktop's selection too. The reducer owns it so that when
 * `remote-control-hgqy` wires the socket, a selection change from the desktop
 * arrives as the same `selection/*` action and the UI cannot drift.
 */
export interface Selection {
  readonly projectId: string;
  readonly sessionId: string;
  readonly terminalId: string;
}

/** An available update (1a's status-bar chip). */
export interface UpdateInfo {
  readonly version: string;
}

/**
 * One coherent picture of the host. This is exactly what `snapshot/received`
 * carries, and exactly what `src/state/fixture.ts` provides today — so
 * swapping the fixture for `ServerMsg::Snapshot` is a change of *source*, not
 * of shape.
 */
export interface Snapshot {
  readonly projects: readonly Project[];
  readonly selection: Selection;
  /** D4: host-owned. The browser letterboxes this; it never negotiates it. */
  readonly geometry: { readonly cols: number; readonly rows: number };
  /** viewer count for the status bar (this tab + desktop = 2 in 1a) */
  readonly viewers: number;
  readonly latencyMs: number | null;
  readonly update: UpdateInfo | null;
}

/** Look-ups used by both the reducer and the components. */
export function findProject(
  projects: readonly Project[],
  projectId: string,
): Project | null {
  return projects.find((p) => p.id === projectId) ?? null;
}

export function findSession(
  projects: readonly Project[],
  projectId: string,
  sessionId: string,
): Session | null {
  const project = findProject(projects, projectId);
  return project?.sessions.find((s) => s.id === sessionId) ?? null;
}
