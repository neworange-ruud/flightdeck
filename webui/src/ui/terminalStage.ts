import type { AppState, TerminalGeometry } from "../state/types";
import { clear, el } from "./dom";
import type { Region } from "./dom";

/**
 * The letterbox. This is the component D4 (turn 2) is about.
 *
 * The desktop owns the PTY grid. `sync_selected_tab_sizes` calls
 * `resize_if_changed` every frame for the selected tab, so any size the browser
 * claimed for itself would be reverted within a frame. So the browser renders
 * the host's grid at its **natural pixel size, centred, with the leftover
 * margin left dark** — it does not scale it, does not fit it, and does not
 * negotiate it.
 *
 * The invariants, which exist to be broken by a well-meaning future commit:
 *   - `@xterm/addon-fit` is not a dependency and must never become one.
 *   - No `transform: scale()` and no `width/height: 100%` on the mount
 *     (see the comment block in `src/style/main.css`).
 *   - `mount` is called once per terminal id, and *never* again on a window
 *     resize. Only a new geometry from the host may remount.
 *
 * `mount` is injected rather than imported so that tests can render the whole
 * screen without a canvas, and so `remote-control-hgqy` can hand in a mount
 * that also subscribes the new terminal to `ServerMsg::Delta`.
 *
 * ## A worker's window (W5, `remote-control-vjtb`)
 *
 * A FlightDeck Worker has no pane of its own, so on a worker the window's pane
 * *is* the terminal's size: `main.ts` measures it and asks the host for that
 * grid, and the grid comes back here. Only then — `hostKind === "worker"` —
 * does a new grid for the same terminal **resize** the mounted terminal
 * (`TerminalHandle.resize`) instead of remounting it, because on a worker the
 * grid follows every drag of the window edge and a remount per step would
 * flash. Every other host keeps the remount, D4 and all of the above.
 */
export type TerminalMount = (
  container: HTMLElement,
  geometry: TerminalGeometry,
  terminalId: string,
) => TerminalHandle | (() => void) | void;

/** What a mount may hand back: how to take it down, and — for a worker's
 * window — how to give it a new grid in place. */
export interface TerminalHandle {
  readonly dispose: () => void;
  readonly resize?: (geometry: TerminalGeometry) => void;
}

export interface TerminalStageOptions {
  /** Which terminal this stage shows, given the state. `null` = nothing yet. */
  readonly terminalId: (state: AppState) => string | null;
  readonly mount: TerminalMount;
  readonly label: string;
  /** The grid to draw `terminalId` at. D4's one host grid by default. */
  readonly geometry?: (state: AppState, terminalId: string) => TerminalGeometry | null;
}

export function createTerminalStage(options: TerminalStageOptions): Region {
  const mountEl = el("div", { class: "fd-mount" });
  const letterbox = el("div", { class: "fd-letterbox" }, [mountEl]);
  const stage = el(
    "div",
    {
      class: "fd-stage",
      attrs: { role: "group", "aria-label": options.label },
    },
    [letterbox],
  );

  let mountedId: string | null = null;
  let mountedCols = -1;
  let mountedRows = -1;
  let handle: TerminalHandle | null = null;

  function render(state: AppState): void {
    const id = options.terminalId(state);
    if (id === null) {
      /** A worker's window can genuinely have nothing to show (no projects
       * yet, no shell). Its terminal is taken down rather than left on screen
       * under a card that says there is none. Any other host never gets here
       * with a mount up, so this changes nothing for it. */
      if (state.hostKind === "worker" && handle !== null) {
        handle.dispose();
        handle = null;
        clear(mountEl);
        mountedId = null;
        mountedCols = -1;
        mountedRows = -1;
      }
      return;
    }
    const geometry =
      options.geometry === undefined ? state.geometry : options.geometry(state, id);
    if (geometry === null) {
      return;
    }
    /** Remount only when the *host* changed something: a different terminal, or
     * a different grid. Never on a container resize — that is the letterbox
     * doing its job, not a reason to touch the PTY's size. */
    const same =
      id === mountedId &&
      geometry.cols === mountedCols &&
      geometry.rows === mountedRows;
    if (same) {
      return;
    }
    /** W5: on a worker the same terminal at a new grid is resized in place.
     * Only there — see the module note. */
    if (
      state.hostKind === "worker" &&
      id === mountedId &&
      handle?.resize !== undefined
    ) {
      handle.resize(geometry);
      mountedCols = geometry.cols;
      mountedRows = geometry.rows;
      return;
    }
    handle?.dispose();
    clear(mountEl);
    handle = asHandle(options.mount(mountEl, geometry, id));
    mountedId = id;
    mountedCols = geometry.cols;
    mountedRows = geometry.rows;
  }

  return { el: stage, update: render };
}

function asHandle(mounted: TerminalHandle | (() => void) | void): TerminalHandle | null {
  if (typeof mounted === "function") {
    return { dispose: mounted };
  }
  return mounted ?? null;
}
