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

import type { DialogState } from "./types";

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

/**
 * What a template [`HostCommand`] expands over, and which `run.args` key the
 * browser fills with the chosen target's id — `protocol::CommandTarget`.
 *
 * `unrecognized` is the host's own `#[serde(other)]` arm arriving here: a
 * target kind this build does not know how to fill in. Such a row is **skipped**
 * rather than rendered without its argument, which is what the Rust side says
 * too — a row that cannot carry its id is a row that cannot be run.
 */
export type CommandTarget =
  | "project"
  | "session"
  | "terminal"
  | "unread_activity"
  | "unrecognized";

/**
 * One row of the host's command inventory (`protocol::CommandView`), as the
 * browser receives it.
 *
 * **The palette has no list of its own** (`remote-control-ll5.12`). The host is
 * the only thing that knows what this build implements, so every row the
 * palette draws is one of these, and a name the host stopped sending stops
 * being offered with no browser change. `hostOnly`, `annotation` and `refusal`
 * are the host's own words — the browser never guesses any of the three.
 */
export interface HostCommand {
  /** Stable id for keyed rendering; equal to `run.name` for a plain row. */
  readonly id: string;
  readonly label: string;
  /** The palette group heading (`Worktree`, `Git`, `Terminals`, …). */
  readonly group: string;
  /** The `Command` frame to send when the row is chosen. */
  readonly run: { readonly name: string; readonly args?: unknown };
  /** D16: the effect lands on the host's machine — `host only`, never hidden. */
  readonly hostOnly: boolean;
  /** D13: this row answers the open dialog (the dialog panel sends it), so it
   * is not a palette row on either surface. */
  readonly answersDialog: boolean;
  /** 1d's right-hand tag, or `null` when the host worded none. */
  readonly annotation: string | null;
  /** Non-null makes this a *template*: one row per target, with the target's id
   * filled into `run.args`. */
  readonly target: CommandTarget | null;
  /** The sentence this build answers with if the row is sent, or `null` when it
   * runs it. Shown as-is; never reworded, never invented. */
  readonly refusal: string | null;
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
  /**
   * D3/D8, `remote-control-ll5.7`: whether the *instance* — desktop included —
   * is in split view. Kept beside `selection` rather than folded into it,
   * because `Selection` here is the browser's local click-target shape
   * (`projectId`/`sessionId`/`terminalId`) and adding a fourth, unrelated flag
   * to it would ripple through every `selection/*` reducer case that spreads
   * it. `AppState.layout` (`ViewLayout`) is derived from this exactly once, in
   * `reduce`'s `snapshot/received` — never guessed at, never flipped locally.
   */
  readonly splitView: boolean;
  /** D4: host-owned. The browser letterboxes this; it never negotiates it. */
  readonly geometry: { readonly cols: number; readonly rows: number };
  /**
   * The raw viewer count the wire carries. 1a drew `2 viewers (this tab +
   * desktop)`; **turn 2 supersedes that** — 2c and 2f both draw `desktop +
   * this tab`, "two named seats, not a counter that implies a crowd". So the
   * chip renders `seats` and this number is only the fallback for a host that
   * sent no seat list.
   */
  readonly viewers: number;
  readonly latencyMs: number | null;
  readonly update: UpdateInfo | null;
  /** D14/2f: the named occupants of the viewer chip. */
  readonly seats: readonly SeatInfo[];
  /**
   * D14: which seat *this* browser got. The host answers this on attach —
   * `Attach { seat: SeatRequest::Control }` can be granted or answered
   * `SeatHeld`, and `SeatRequest::Observe` is granted read-only — so the
   * browser is told rather than assuming, and never paints a mode it does not
   * have.
   */
  readonly seat: Seat;
  /**
   * D11's backfill. The host retains `min(200 events, 24h)` and replays it on
   * attach (`Snapshot::activity`), **oldest first**, so a fresh tab opens on
   * history rather than silence. The feed renders it newest first.
   */
  readonly activity: readonly ActivityEvent[];
  /**
   * D13: the one open dialog, or `null` when there is none.
   *
   * On the snapshot rather than only on a delta, because a dialog is app state:
   * a tab that attaches while the desktop has one open must paint it, and it
   * never saw the `Delta::DialogOpened`. `null` is the host saying there is
   * none — never a guess.
   */
  readonly dialog: DialogState | null;
  /**
   * The palette's whole inventory (`remote-control-ll5.12`), in the host's own
   * display order.
   *
   * On the snapshot because it is static for the life of the host build. Empty
   * from a host that sends none, which is an empty palette — the browser has no
   * list of its own to fall back to, by design.
   */
  readonly commands: readonly HostCommand[];
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

/* ===========================================================================
 * Turn 2 — the states artboards 2b/2c/2d/2e/2f render (remote-control-l7ya).
 *
 * Same rule as everything above: a *view* model, mirroring the host types by
 * name so `remote-control-hgqy` maps rather than translates. Where a Rust type
 * already decides something (`AccessScreen`, `ActivityTier`, `ShutdownReason`,
 * `Seat`), the union below has exactly its variants — no fifth screen, no
 * re-derived precedence.
 * ======================================================================== */

/**
 * The four browser-side access screens (2b).
 *
 * Mirrors `src/web/credentials.rs`'s `AccessScreen` variant for variant —
 * `CodeEntry | Rejected | Revoked | RateLimited` — and the server sends exactly
 * these spellings in the refusal body's `screen` field
 * (`screen_name` in `src/web/server.rs`). 2b draws three panels and puts the
 * rate-limit state in the third's footer strip; the host models it as a fourth
 * screen because it is the one state where retrying is *refused* rather than
 * merely useless, so the browser must stop offering the button.
 */
export type AccessScreen =
  | "code_entry"
  | "rejected"
  | "revoked"
  | "rate_limited";

/** The bootstrap code is four digits (`BOOTSTRAP_CODE_LEN`, 2a's `8412`). */
export const ACCESS_CODE_LENGTH = 4;

/**
 * Everything the four 2b screens render.
 *
 * `attemptsRemaining` and `lockoutSeconds` are the host's numbers, not ours:
 * `CredentialStore::attempts_remaining` and `AuthFailure::RateLimited {
 * retry_after_ms }` arrive in the `POST /auth/exchange` refusal body. The
 * browser must never compute its own attempt budget — it would disagree with
 * the limiter that actually decides.
 */
export interface AccessState {
  readonly screen: AccessScreen;
  /** Digits typed so far, at most `ACCESS_CODE_LENGTH`. */
  readonly code: string;
  /** The digits that were refused, kept so 2b's rejected screen can show them. */
  readonly refused: string;
  /** From the host. `null` before any refusal has been seen. */
  readonly attemptsRemaining: number | null;
  /** Seconds until this address may try again; `null` unless rate-limited. */
  readonly lockoutSeconds: number | null;
  /** 2b's revoked screen: `withdrew this browser's access 12s ago`. A label,
   * never a computed duration — the reducer has no clock. */
  readonly revokedAgo: string | null;
}

/**
 * Why the socket closed for good (Q5) — mirrors `ShutdownReason` in
 * `src/web/protocol.rs`, including its `unknown` catch-all.
 */
export type ShutdownReason =
  | "host_quit"
  | "server_stopped"
  | "token_revoked"
  | "restarting"
  | "unknown";

/**
 * A received `ServerMsg::Shutdown`. Q5's whole point: this is *not* a network
 * failure, so the browser must not show `reconnecting`.
 */
export interface ShutdownState {
  readonly reason: ShutdownReason;
  /**
   * True when the quit came from a `Command` sent by **this** browser. Q5: the
   * browser then acknowledges the user's own action instead of reporting a
   * failure — "you quit FlightDeck from this tab" (2c), not "the host stopped
   * answering".
   */
  readonly selfInitiated: boolean;
  /**
   * The host's own words, rendered verbatim. `Shutdown { detail }` is an
   * `Option<String>` on the wire; map `None` to `""` rather than to a sentence
   * of ours, so "the host said nothing" stays distinguishable from "the host
   * said something".
   */
  readonly detail: string;
  /**
   * 2c's `host exited cleanly 16:42`.
   *
   * A label, and one the **browser** stamps: `Shutdown` carries no timestamp,
   * and it does not need to — the frame arrives when it arrives, so the moment
   * it is received *is* the moment the host went away. `""` renders no time.
   */
  readonly atLabel: string;
}

/**
 * Q5 / `ShutdownReason::should_retry`: **only a restart is worth retrying.** An
 * unknown reason says no, because the honest default for "the host said
 * something final we do not understand" is to stop and say so, not to spin.
 * Mirrored here rather than re-decided, so the two halves cannot disagree.
 */
export function shouldRetry(reason: ShutdownReason): boolean {
  return reason === "restarting";
}

/**
 * A host that updated under an open tab (2c, turn 2 §4). Detected by the wire's
 * `check_version()` / `ErrorCode::VersionMismatch`; the answer is a reload, not
 * a retry, because the SPA ships inside the binary (D9) and so a version
 * difference is a stale tab rather than a negotiation.
 *
 * Note what 2c draws: the connection stays `● connected 21ms` and the mode chip
 * keeps its mode. Nothing about control was lost — the tab is merely old.
 */
export interface VersionMismatch {
  /** The version this tab was built from, e.g. `v1.16.0`. */
  readonly tabVersion: string;
  /** The version the host now runs, e.g. `v1.17.0`. */
  readonly hostVersion: string;
}

/**
 * How to fill `VersionMismatch` without inventing a build stamp.
 *
 * There is no compile-time version baked into this bundle, and there does not
 * need to be: **the SPA ships inside the binary (D9)**, so the version this tab
 * was built from is exactly the `Snapshot { host_version }` it *first* attached
 * with. Remember that value, compare it with the `host_version` on every later
 * snapshot, and a difference means the host was updated underneath an open tab
 * — which is precisely what 2c's row says.
 *
 * The protocol half is separate and stricter: `check_version()` answering
 * `ErrorCode::VersionMismatch` means this tab cannot speak to this host at all,
 * and the frame's `WireError { version }` payload carries the numbers. Both
 * paths end at the same row, because both have the same fix — reload.
 *
 * `remote-control-hgqy` owns the dispatch; this helper is the recipe, so the
 * comparison is written once.
 */
export function versionMismatchBetween(
  attachedWith: string,
  hostNow: string,
): VersionMismatch | null {
  if (attachedWith === "" || hostNow === "" || attachedWith === hostNow) {
    return null;
  }
  return { tabVersion: attachedWith, hostVersion: hostNow };
}

/**
 * 2d's frozen clock. The terminal is a photograph, and the two facts that make
 * that legible are *when* it was taken and how long ago.
 *
 * Both are pre-formatted labels: the reducer is clock-free by construction and
 * a component reading `Date.now()` would be the same impurity one layer down.
 * The transport formats them when it dispatches.
 */
export interface Staleness {
  /** 2d's header clock, e.g. `16:41:08` — frozen, not ticking. */
  readonly frozenAt: string;
  /** 2d's `frozen 34s ago`, and the strip's `terminal stale 34s`. */
  readonly ago: string;
}

/**
 * 2d's catching-up pane: `replaying 41 KB…` over a progress bar, and `live
 * again · replaying what you missed from byte 1 204 992`.
 *
 * The byte offset is Q3's cursor, so this is a real protocol fact rather than a
 * decorative percentage.
 */
export interface ReplayProgress {
  readonly bytesDone: number;
  readonly bytesTotal: number;
  /** The cursor the host resumed from (Q3). */
  readonly fromByte: number;
  /** Q3: the ring aged out, so continuity is broken and we must say so. */
  readonly truncated: boolean;
}

/** Mirrors `protocol::Seat`: one controller, N observers (D14). */
export type Seat = "controlling" | "observing";

/**
 * One occupant of the viewer chip — mirrors `protocol::SeatInfo`.
 *
 * 2f: the chip reads `desktop + this tab`, **two named seats, not a counter
 * that implies a crowd.** So this carries a label to render verbatim, never a
 * number to total up.
 */
export interface SeatInfo {
  readonly label: string;
  readonly seat: Seat;
  /** The desktop is not a viewer (`SeatInfo::viewer_id == null`). */
  readonly isDesktop: boolean;
  /** `14 minutes, active 20s ago` — a label; see `Staleness` for why. */
  readonly sinceLabel: string;
}

/**
 * The browser that currently holds the controlling seat, as 2f's arriving
 * panel names it. Exactly the identifying detail turn 2 calls fair: where it
 * is, what it is, and how long it has been there.
 */
export interface Incumbent {
  readonly address: string;
  readonly browser: string;
  readonly connected: string;
}

/**
 * The takeover prompt (2f), in its two directions.
 *
 * Two protocol facts shape this:
 *   - **Takeover has no dedicated frame.** The client re-sends `Attach { seat:
 *     SeatRequest::TakeOver }`; there is no `ClientMsg::TakeOver` to model.
 *   - **Eviction is a `Delta::Seats`, never a `Shutdown`.** The evicted socket
 *     stays open as an observer, which is why `evicted` is a prompt over a live
 *     connection rather than a terminal state.
 *
 * Neither direction is a permission check. D14: anyone holding the credential
 * can evict anyone; this is courtesy, so that neither person wonders why the
 * keys stopped working.
 */
export type TakeoverState =
  /** We asked for control and the seat is held (`ErrorCode::SeatHeld`). */
  | { readonly kind: "arriving"; readonly incumbent: Incumbent }
  /** We *had* control and a `Delta::Seats` took it away. */
  | {
      readonly kind: "evicted";
      /** The browser that took it, e.g. `192.168.2.11`. */
      readonly byAddress: string;
      /** 2f: `the last one that landed was 3s ago`. */
      readonly lastInputAgo: string;
    };

/**
 * Urgency of one feed entry — mirrors `protocol::ActivityTier` exactly, and its
 * precedence is mirrored (not re-derived) in `src/state/activity.ts`.
 */
export type ActivityTier = "attention" | "finished" | "quiet";

/**
 * One row of the activity feed (2e) — mirrors `protocol::ActivityEvent`.
 *
 * `reason` is the part a user actually reads (`asked a question`, `agent exited
 * (code 1)`, `finished, 18 files touched`) and it comes **from the host**: the
 * browser could not reconstruct "18 files touched" from `from`/`to` at all.
 * It is empty when the host has nothing honest to say, and **must never be
 * padded with a guess** — the `unknown → unknown · Codex CLI reports no
 * lifecycle` row is data, not a fallback string invented here.
 */
export interface ActivityEvent {
  readonly id: string;
  /** `40s ago` — a label; the host's `at_ms` formatted by the caller. */
  readonly atLabel: string;
  readonly projectId: string;
  readonly projectName: string;
  readonly sessionId: string;
  readonly sessionName: string;
  readonly from: SessionStatus;
  readonly to: SessionStatus;
  /** Verbatim from the host. Empty is a legal value and renders as nothing. */
  readonly reason: string;
  readonly tier: ActivityTier;
  /** The host's own record, so a fresh tab opens on history, not silence. */
  readonly read: boolean;
}
