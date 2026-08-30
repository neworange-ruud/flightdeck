import { findSession } from "../state/model";
import type { AppState } from "../state/types";
import { clear, el } from "./dom";
import type { Region } from "./dom";
import { createTerminalStage } from "./terminalStage";
import type { TerminalMount } from "./terminalStage";
import type { Store } from "./store";

/**
 * Artboard 1c — split view, three terminals side by side.
 *
 * What 1c changes and what it does not: *"the tab bar is replaced by per-column
 * label rows · focus glow tracks the active column"*. So the tab bar is swapped
 * for label rows and each column letterboxes its own grid — everything else
 * (logo band, project tabs, sidebar, git bar, status bar) stays exactly where it
 * was. 1c's own frame is drawn 560px tall and omits the project row and git
 * bar; that is the artboard cropping for space on the canvas, not a claim that
 * the branch and the geometry chip stop being facts in split view.
 *
 * Toggling split *from the browser* is out of M1 (D8), so nothing in M1
 * dispatches `layout/set` — the layout is state, and this renders it, which is
 * how M2 gets the feature for the price of a keybinding.
 *
 * Each column letterboxes independently. That is not a compromise: the host
 * owns three grids, and three natural-size grids centred in three columns is
 * the only rendering that does not lie about any of them (D4).
 */
export function createSplitView(store: Store, mount: TerminalMount): Region {
  const root = el("div", {
    class: "fd-split",
    attrs: { role: "group", "aria-label": "Split view" },
  });

  /** Columns are keyed by *index*, so the stages survive re-renders and are not
   * remounted (which would clear a live terminal) when only focus moves. */
  const columns = new Map<number, { el: HTMLElement; update: (s: AppState) => void }>();
  let builtFor: string | null = null;

  function render(state: AppState): void {
    const selection = state.selection;
    const session =
      selection === null
        ? null
        : findSession(state.projects, selection.projectId, selection.sessionId);
    const terminals = session?.terminals ?? [];
    const key = `${session?.id ?? ""}:${terminals.map((t) => t.id).join(",")}`;

    if (key !== builtFor) {
      clear(root);
      columns.clear();
      builtFor = key;
      terminals.forEach((terminal, index) => {
        const stage = createTerminalStage({
          terminalId: () => terminal.id,
          mount,
          label: `terminal ${terminal.title}`,
        });
        const label = el("div", { class: "fd-column__label" }, [
          el("span", { text: terminal.title }),
          el("span", {
            class: "fd-column__index",
            text: `${index + 1}/${terminals.length}`,
          }),
        ]);
        const column = el(
          "div",
          {
            class: "fd-column",
            attrs: { "data-focused": "false", "data-kind": terminal.kind },
          },
          [label, stage.el],
        );
        column.addEventListener("click", () => {
          store.dispatch({ type: "split/focus", column: index });
          store.dispatch({ type: "selection/terminal", terminalId: terminal.id });
        });
        columns.set(index, {
          el: column,
          update: (s) => {
            column.setAttribute(
              "data-focused",
              String(s.splitFocus === index),
            );
            stage.update(s);
          },
        });
        root.append(column);
      });
    }

    for (const column of columns.values()) {
      column.update(state);
    }
  }

  return { el: root, update: render };
}
