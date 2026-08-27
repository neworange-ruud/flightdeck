import type { AppAction, AppState } from "./types";

/**
 * The one entry point later tasks dispatch through. Pure by construction: no
 * network, no DOM, no `Date.now()`/`Math.random()` — every input is in
 * `action`, every output is a new `AppState`. That purity is what D15 asks
 * `vitest` to exercise directly, with no server, socket or browser involved.
 *
 * Side effects (actually sending queued input over the websocket once
 * connected, actually constructing/resizing the xterm.js instance when
 * `geometry` changes) belong to the caller, not here — they read the new
 * state after `reduce` returns and act on it. Keeping that boundary is what
 * lets this function stay unit-testable once `src/web/protocol.rs` lands and
 * real actions replace these placeholders.
 */
export function reduce(state: AppState, action: AppAction): AppState {
  switch (action.type) {
    case "connection/changed":
      return { ...state, connection: action.status };

    case "geometry/set":
      return { ...state, geometry: action.geometry };

    case "input/queue":
      return { ...state, pendingInput: [...state.pendingInput, action.data] };

    case "input/flush":
      return state.pendingInput.length === 0
        ? state
        : { ...state, pendingInput: [] };

    default: {
      // Exhaustiveness check: a new AppAction variant left unhandled above is
      // a compile error here, not a silently-ignored action at runtime.
      const unreachable: never = action;
      return unreachable;
    }
  }
}
