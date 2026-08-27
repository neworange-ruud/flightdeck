import { reduce } from "../state/reducer";
import type { AppAction, AppState } from "../state/types";

/**
 * A subscribe/dispatch shell around `reduce`. All state changes in the app go
 * through here; nothing keeps state in the DOM, which is what makes the
 * regions re-renderable from a snapshot and testable without a browser.
 *
 * `onDispatch` is the seam for the wire. D3 makes selection *instance-wide*, so
 * a click in the browser has to reach the host as well as the local state; the
 * store therefore hands every action to an optional listener after reducing it.
 * `remote-control-hgqy` passes a function there that turns `selection/*` into
 * `ClientMsg::Command` and `input/*` into `ClientMsg::Input` frames. Until then
 * `src/ui/app.ts` passes a logger, so the intent is visible without pretending
 * the desktop has been told.
 */
export interface Store {
  getState(): AppState;
  dispatch(action: AppAction): void;
  /** Returns an unsubscribe function. */
  subscribe(listener: (state: AppState) => void): () => void;
}

export interface StoreOptions {
  /** Called after every reduction, with the action and the resulting state. */
  readonly onDispatch?: (action: AppAction, state: AppState) => void;
}

export function createStore(
  initial: AppState,
  options: StoreOptions = {},
): Store {
  let state = initial;
  const listeners = new Set<(state: AppState) => void>();

  return {
    getState: () => state,
    dispatch(action) {
      const next = reduce(state, action);
      const changed = next !== state;
      state = next;
      options.onDispatch?.(action, state);
      if (changed) {
        for (const listener of listeners) {
          listener(state);
        }
      }
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}
