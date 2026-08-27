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
 * remote-control-sk4u (the main screen) extends this shape; it should not
 * need to replace it wholesale.
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

/** The whole app's reducer-owned state. Deliberately small: this is the seam,
 * not the final shape. Add fields as later tasks need them rather than
 * pre-guessing the eventual `Snapshot`. */
export interface AppState {
  readonly connection: ConnectionStatus;
  /** `null` until the first `Snapshot` arrives (D4/D12) — there is no
   * sensible default grid size to invent locally. */
  readonly geometry: TerminalGeometry | null;
  /** Keystrokes typed while not fully connected. FIFO order matters: this is
   * what "queued, never dropped, never reordered" (turn 2 §5.1) means at the
   * state layer. Drained by `input/flush` once the link is ready. */
  readonly pendingInput: readonly string[];
}

export function createInitialState(): AppState {
  return {
    connection: "connecting",
    geometry: null,
    pendingInput: [],
  };
}

export type AppAction =
  | { readonly type: "connection/changed"; readonly status: ConnectionStatus }
  | { readonly type: "geometry/set"; readonly geometry: TerminalGeometry }
  | { readonly type: "input/queue"; readonly data: string }
  | { readonly type: "input/flush" };
