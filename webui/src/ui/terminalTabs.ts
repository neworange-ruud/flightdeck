import { findSession } from "../state/model";
import type { TerminalTab } from "../state/model";
import type { AppState } from "../state/types";
import { clear, el, separator } from "./dom";
import type { Region } from "./dom";
import type { Store } from "./store";

/**
 * Region 4 of 7 — the terminal tab bar (1a: `agent`, `shell 1`, `shell 2`,
 * then `+ agent` and `+ shell`).
 *
 * The selected tab is filled, not outlined: `--fd-focus` for the agent tab and
 * `--fd-accent` for a shell, per 2g's own assignments ("agent tab" / "shell
 * tab"). Unselected labels take `--fd-term-asleep` — the tone that means "this
 * terminal exists but is not the one you are reading".
 *
 * `+ agent` opens the new-agent dialog (artboard 1e) and `+ shell` creates a
 * terminal. Neither is `host only`: D13 makes dialogs *shared* with an origin
 * label, so their effect lands in the browser too — and since
 * `remote-control-ll5.3` the host really does open a shared dialog for both
 * (`new_agent`, `new_child_terminal`).
 *
 * They stay disabled for a different reason, and the titles say which: this
 * build's palette inventory is still the curated local list in
 * `state/commands.ts`, not `Snapshot::commands` (R7). Inventing a wire name here
 * is exactly what R7 stopped the SPA doing, so these two buttons wake up when
 * `remote-control-ll5.1`'s inventory reaches the browser — one change, in one
 * place, for every row rather than for these two.
 */
export function createTerminalTabs(store: Store): Region {
  const bar = el("div", {
    class: "fd-tabs",
    attrs: { role: "tablist", "aria-label": "Terminals" },
  });

  function render(state: AppState): void {
    clear(bar);
    const selection = state.selection;
    const session =
      selection === null
        ? null
        : findSession(state.projects, selection.projectId, selection.sessionId);

    (session?.terminals ?? []).forEach((terminal, index) => {
      if (index > 0) {
        bar.append(separator());
      }
      bar.append(
        terminalTab(terminal, terminal.id === selection?.terminalId, store),
      );
    });

    bar.append(
      el("div", { class: "fd-spacer" }),
      el("button", {
        class: "fd-action fd-action--new-agent",
        text: "+ agent",
        title:
          "the host opens a shared dialog for this (D13); the browser reaches it \
once the palette reads Snapshot::commands (remote-control-ll5.1)",
        attrs: { type: "button", disabled: "" },
      }),
      el("button", {
        class: "fd-action fd-action--new-shell",
        text: "+ shell",
        title:
          "the host runs this (new_child_terminal); the browser reaches it once \
the palette reads Snapshot::commands (remote-control-ll5.1)",
        attrs: { type: "button", disabled: "" },
      }),
    );
  }

  return { el: bar, update: render };
}

function terminalTab(
  terminal: TerminalTab,
  selected: boolean,
  store: Store,
): HTMLElement {
  const label = el("button", {
    class: "fd-tab__label",
    text: terminal.title,
    title: "select this terminal — this also moves the desktop's selection",
    attrs: {
      type: "button",
      role: "tab",
      "aria-selected": String(selected),
    },
  });
  label.addEventListener("click", () => {
    store.dispatch({ type: "selection/terminal", terminalId: terminal.id });
  });

  const close = el("button", {
    class: "fd-tab__close",
    text: "✕",
    title:
      "closing a terminal opens a shared confirmation (D13); the browser reaches \
it once the palette reads Snapshot::commands (remote-control-ll5.1)",
    attrs: {
      type: "button",
      disabled: "",
      "aria-label": `Close ${terminal.title}`,
    },
  });

  return el(
    "div",
    {
      class: "fd-tab",
      attrs: {
        role: "presentation",
        "data-selected": String(selected),
        "data-kind": terminal.kind,
      },
    },
    [label, close],
  );
}
