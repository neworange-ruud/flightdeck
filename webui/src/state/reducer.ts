import { decideEscape } from "../input/escape";
import { findProject } from "./model";
import type { Project, Selection } from "./model";
import type { AppAction, AppState } from "./types";

/**
 * The one entry point later tasks dispatch through. Pure by construction: no
 * network, no DOM, no `Date.now()`/`Math.random()` — every input is in
 * `action`, every output is a new `AppState`. That purity is what D15 asks
 * `vitest` to exercise directly, with no server, socket or browser involved.
 *
 * Side effects (actually sending queued input over the websocket once
 * connected, actually constructing/resizing the xterm.js instance when
 * `geometry` changes, telling the host about a selection change so the desktop
 * follows it per D3) belong to the caller, not here — they read the new state
 * after `reduce` returns and act on it. Keeping that boundary is what lets
 * this function stay unit-testable once `src/web/protocol.rs` lands and real
 * actions replace these placeholders.
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

    case "snapshot/received": {
      const { snapshot } = action;
      return {
        ...state,
        projects: snapshot.projects,
        /** D3: the host's selection wins. A browser that kept its own would be
         * a second source of truth, which is the drift this decision removes. */
        selection: snapshot.selection,
        geometry: snapshot.geometry,
        viewers: snapshot.viewers,
        latencyMs: snapshot.latencyMs,
        update: snapshot.update,
      };
    }

    case "selection/project": {
      const project = findProject(state.projects, action.projectId);
      if (project === null) {
        return state;
      }
      return { ...state, selection: firstSelectionIn(project) };
    }

    case "selection/session": {
      if (state.selection === null) {
        return state;
      }
      const project = findProject(state.projects, state.selection.projectId);
      const session =
        project?.sessions.find((s) => s.id === action.sessionId) ?? null;
      if (project === null || session === null) {
        return state;
      }
      return {
        ...state,
        selection: {
          projectId: project.id,
          sessionId: session.id,
          /** Selecting a session selects its first terminal: the agent tab.
           * A selection that named a terminal from the previous session would
           * point at nothing. */
          terminalId: session.terminals[0]?.id ?? "",
        },
      };
    }

    case "selection/terminal": {
      if (state.selection === null) {
        return state;
      }
      return {
        ...state,
        selection: { ...state.selection, terminalId: action.terminalId },
      };
    }

    case "mode/set":
      /** Leaving terminal focus also closes any half-typed `Esc Esc` chord —
       * an armed window that outlived its mode would fire on the next visit. */
      return { ...state, mode: action.mode, escArmedAt: null };

    case "layout/set":
      return { ...state, layout: action.layout };

    case "split/focus":
      return { ...state, splitFocus: action.column };

    case "input/esc": {
      const outcome = decideEscape(state.escArmedAt, action.at);
      if (outcome.kind === "leave_focus") {
        return { ...state, mode: "app", escArmedAt: null };
      }
      /**
       * §5: a single `Esc` still reaches the agent — `esc to interrupt` is the
       * key users press most. It is queued like any other keystroke so the
       * "never dropped, never reordered" guarantee (§5.1) covers it too.
       */
      return {
        ...state,
        pendingInput: [...state.pendingInput, "\x1b"],
        escArmedAt: outcome.armedAt,
      };
    }

    default: {
      // Exhaustiveness check: a new AppAction variant left unhandled above is
      // a compile error here, not a silently-ignored action at runtime.
      const unreachable: never = action;
      return unreachable;
    }
  }
}

function firstSelectionIn(project: Project): Selection {
  const session = project.sessions[0];
  return {
    projectId: project.id,
    sessionId: session?.id ?? "",
    terminalId: session?.terminals[0]?.id ?? "",
  };
}
