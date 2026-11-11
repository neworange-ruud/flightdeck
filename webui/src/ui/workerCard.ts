import { shellExitLine, workerCard } from "../state/worker";
import type { AppState } from "../state/types";
import { clear, el } from "./dom";
import type { Region } from "./dom";
import type { Store } from "./store";

/**
 * What a worker's main area says when there is no terminal to show, or when
 * the one on screen has ended (`remote-control-vjtb`):
 *
 * - **No projects yet** — a fresh worker opens on an empty workspace, and the
 *   honest thing is a card that says so with the one action that changes it:
 *   Open Project (the picker). The native window's words.
 * - **The worker shell is not running** — its tab, with Restart shell.
 * - **The worker shell exited** — a bar under the shell, which stays on screen
 *   (it is the last thing it printed), with Restart shell.
 *
 * Hidden on every other host and whenever a terminal is simply on screen, so a
 * TUI or Desktop host never draws any of it. The layout consequence — the pane
 * gives way to a card — is `data-worker-card` on the frame (`ui/app.ts`).
 */
export function createWorkerCard(store: Store): Region {
  const box = el("div", {
    class: "fd-workercard",
    attrs: { role: "status", "aria-live": "polite" },
  });

  function render(state: AppState): void {
    const card = workerCard(state);
    box.hidden = card === null;
    if (card === null) {
      box.removeAttribute("data-kind");
      return;
    }
    box.setAttribute("data-kind", card);
    clear(box);
    switch (card) {
      case "empty":
        box.append(
          el("div", { class: "fd-workercard__panel" }, [
            el("p", {
              class: "fd-workercard__title",
              text: "This worker has no projects yet.",
            }),
            el("p", {
              class: "fd-workercard__detail",
              text: "Open a repository on it, or clone one.",
            }),
            button("Open Project", "open-project", () =>
              store.dispatch({ type: "picker/request", intent: { kind: "open" } }),
            ),
          ]),
        );
        return;
      case "shell_missing":
        box.append(
          el("div", { class: "fd-workercard__panel" }, [
            el("p", {
              class: "fd-workercard__title",
              text: shellExitLine(null),
            }),
            restart(store),
          ]),
        );
        return;
      case "shell_exited":
        box.append(
          el("div", { class: "fd-workercard__bar" }, [
            el("span", {
              class: "fd-workercard__line",
              text: shellExitLine(state.worker?.shell ?? null),
            }),
            restart(store),
          ]),
        );
        return;
    }
  }

  return { el: box, update: render };
}

function restart(store: Store): HTMLElement {
  return button("Restart shell", "restart-shell", () =>
    store.dispatch({ type: "workerShell/restart" }),
  );
}

function button(label: string, action: string, onClick: () => void): HTMLElement {
  const node = el("button", {
    class: "fd-workercard__action",
    text: label,
    attrs: { type: "button", "data-action": action },
  });
  node.addEventListener("click", onClick);
  return node;
}
