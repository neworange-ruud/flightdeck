import type { Project, Selection, Snapshot, UpdateInfo } from "./model";

/**
 * App-state seam for the web SPA.
 *
 * These types are deliberately local and minimal, NOT imported from a
 * protocol module — `src/web/protocol.rs` (D12) is being written by a
 * concurrent task and does not exist as shippable TypeScript yet. When it
 * lands, expect:
 *
 *   - `ConnectionStatus` to be replaced/derived from the wire's connection
 *     lifecycle (`ServerMsg::Ack` / transport events), which turn 2 also
 *     requires a `catching_up` state for (§5.1: "input queues until the
 *     replay lands").
 *   - `TerminalGeometry` to arrive on `ServerMsg::Snapshot` /
 *     `ServerMsg::Delta` (host-owned cols/rows, D4) instead of being set
 *     locally.
 *   - Queued input (`pendingInput` below) to be flushed as
 *     `ClientMsg::Input` frames, preserving order — D15 turn 2 §5.1 requires
 *     keystrokes to be "queued, never dropped" and never reordered once the
 *     link returns.
 *
 * remote-control-sk4u (the main screen) extended this shape rather than
 * replacing it: the scaffold's five fields are all still here and still mean
 * what they meant. What it added is the *content* of artboards 1a–1c —
 * projects, the shared selection (D3), the mode chip and the view layout —
 * plus one `snapshot/received` action, which is the single seam
 * `remote-control-hgqy` needs to swap the fixture for the live socket.
 */

/** Mirrors the *coarse* lifecycle the status bar renders (turn 2, 2c/2d).
 * Real wire-level detail (byte cursors, ack sequence numbers) belongs to
 * `src/web/protocol.rs`, not here — the UI only needs to know which of these
 * coarse buckets it is in. */
export type ConnectionStatus =
  | "connecting"
  | "connected"
  | "reconnecting"
  | "catching_up"
  | "disconnected";

/**
 * The PTY grid size. D4: the desktop always owns this — the browser never
 * negotiates or requests a size, it only ever *receives* one and letterboxes
 * around it (turn 2 revision: no scaling, no `FitAddon`). See
 * `src/term/terminal.ts` for the rendering side of that invariant.
 */
export interface TerminalGeometry {
  readonly cols: number;
  readonly rows: number;
}

/**
 * Which surface has the keyboard (1a vs 1b).
 *
 * `terminal` — keystrokes go to the PTY; the viewport carries the focus glow.
 * `app` — keystrokes go to FlightDeck; the sidebar carries the glow and the
 * terminal renders asleep (`--fd-term-asleep`, 2d).
 *
 * §5.1 adds a third *rendered* value that is not a mode: any state that costs
 * the user control drains the chip to `MODE: —`, because the mode is a lie
 * while input is not arriving. That is derived from `connection`, not stored —
 * see `modeChipLabel` in `src/ui/statusBar.ts`.
 */
export type UiMode = "terminal" | "app";

/** Single viewport (1a/1b) or the three-column split (1c). Toggling split from
 * the browser is M2 (D8), so nothing in M1 dispatches `layout/set` except
 * tests — the state exists so 1c is a render of state, not a second app. */
export type ViewLayout = "single" | "split";

/** The whole app's reducer-owned state. */
export interface AppState {
  readonly connection: ConnectionStatus;
  /** `null` until the first `Snapshot` arrives (D4/D12) — there is no
   * sensible default grid size to invent locally. */
  readonly geometry: TerminalGeometry | null;
  /** Keystrokes typed while not fully connected. FIFO order matters: this is
   * what "queued, never dropped, never reordered" (turn 2 §5.1) means at the
   * state layer. Drained by `input/flush` once the link is ready. */
  readonly pendingInput: readonly string[];

  /** Everything the host told us about, empty until the first snapshot. */
  readonly projects: readonly Project[];
  /**
   * D3: the *instance-wide* selection, not a browser-local one. A selection
   * made here is also the desktop's selection; a selection made on the desktop
   * arrives as the same action. `null` only before the first snapshot.
   */
  readonly selection: Selection | null;
  readonly mode: UiMode;
  readonly layout: ViewLayout;
  /** Column index with focus in split view (1c: the glow tracks it). */
  readonly splitFocus: number;
  readonly viewers: number;
  readonly latencyMs: number | null;
  readonly update: UpdateInfo | null;
  /**
   * When the last unpaired `Esc` was seen, for the 400 ms `Esc Esc` window
   * (§5). Stored rather than kept in a component so the decision is a pure
   * reduction; the timestamp always arrives on the action, never from a clock
   * read inside the reducer. `null` means no window is open.
   */
  readonly escArmedAt: number | null;
}

export function createInitialState(): AppState {
  return {
    connection: "connecting",
    geometry: null,
    pendingInput: [],
    projects: [],
    selection: null,
    /** Nothing is focused before the first snapshot, and App mode is the
     * honest default: the app cannot promise keystrokes reach a PTY it has not
     * heard about yet. */
    mode: "app",
    layout: "single",
    splitFocus: 0,
    viewers: 0,
    latencyMs: null,
    update: null,
    escArmedAt: null,
  };
}

export type AppAction =
  | { readonly type: "connection/changed"; readonly status: ConnectionStatus }
  | { readonly type: "geometry/set"; readonly geometry: TerminalGeometry }
  | { readonly type: "input/queue"; readonly data: string }
  | { readonly type: "input/flush" }
  /**
   * One coherent picture of the host replaces the previous one. Today the
   * fixture dispatches it once at boot; `remote-control-hgqy` dispatches it on
   * every `ServerMsg::Snapshot`. Deliberately whole-picture rather than a
   * dozen fine-grained actions: the host is the source of truth (D3) and a
   * partial update is how two surfaces drift apart.
   */
  | { readonly type: "snapshot/received"; readonly snapshot: Snapshot }
  /**
   * D3: selecting for the whole instance. The reducer only moves local state;
   * the caller is responsible for telling the host, which is where
   * `ClientMsg::Command { select_session }` goes (see `src/ui/app.ts`).
   */
  | { readonly type: "selection/project"; readonly projectId: string }
  | { readonly type: "selection/session"; readonly sessionId: string }
  | { readonly type: "selection/terminal"; readonly terminalId: string }
  | { readonly type: "mode/set"; readonly mode: UiMode }
  | { readonly type: "layout/set"; readonly layout: ViewLayout }
  | { readonly type: "split/focus"; readonly column: number }
  /**
   * An `Esc` arrived while the terminal had focus. `at` is a timestamp the
   * caller read (`performance.now()`), which keeps `reduce` pure: the 400 ms
   * window is evaluated by `decideEscape`, and either the key is queued for
   * the agent or focus is released.
   */
  | { readonly type: "input/esc"; readonly at: number };
