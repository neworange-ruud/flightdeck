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
 */
export type TerminalMount = (
  container: HTMLElement,
  geometry: TerminalGeometry,
  terminalId: string,
) => (() => void) | void;

export interface TerminalStageOptions {
  /** Which terminal this stage shows, given the state. `null` = nothing yet. */
  readonly terminalId: (state: AppState) => string | null;
  readonly mount: TerminalMount;
  readonly label: string;
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
  let dispose: (() => void) | void;

  function render(state: AppState): void {
    const id = options.terminalId(state);
    const geometry = state.geometry;
    if (id === null || geometry === null) {
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
    if (typeof dispose === "function") {
      dispose();
    }
    clear(mountEl);
    dispose = options.mount(mountEl, geometry, id);
    mountedId = id;
    mountedCols = geometry.cols;
    mountedRows = geometry.rows;
  }

  return { el: stage, update: render };
}
