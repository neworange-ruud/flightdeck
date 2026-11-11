import { TAKEN_OVER_DETAIL, takenOverTitle } from "../state/connection";
import type { AppState } from "../state/types";
import { clear, el } from "./dom";
import type { Region } from "./dom";

/**
 * A worker's window that another window took over (`specs/WORKER_PRD.md` W17,
 * `remote-control-vjtb`).
 *
 * A worker serves one window at a time, and the newest attach wins: the host
 * sends this window `taken_over` and closes the socket. Nothing here redials —
 * attaching as a window again would take the worker straight back from whoever
 * just took it, and two tabs doing that to each other would fight for ever. So
 * the screen names who took it and offers the one action that works, **Take it
 * back**, which is a real reconnect and does the same thing to them.
 *
 * Drawn with 2f's panel (`fd-takeover`), because it is the same kind of news —
 * someone else has the keys now — told in the native window's words
 * (`desktop/src/remote/window.rs`).
 */
export interface TakenOverOptions {
  readonly onReclaim?: () => void;
}

export function createTakenOver(options: TakenOverOptions = {}): Region {
  const panel = el("div", { class: "fd-takeover__panel" });
  const layer = el(
    "div",
    {
      class: "fd-takeover fd-takenover",
      attrs: { role: "dialog", "aria-label": "Taken over" },
    },
    [panel],
  );

  function render(state: AppState): void {
    const shown = state.connection === "taken_over";
    layer.hidden = !shown;
    if (!shown) {
      return;
    }
    clear(panel);
    const button = el(
      "button",
      {
        class: "fd-takeover__action fd-takeover__action--primary",
        attrs: { type: "button", "data-key": "Enter", "data-action": "reclaim" },
      },
      [el("span", { class: "fd-key-box", text: "Enter" }), "Take it back"],
    );
    button.addEventListener("click", () => options.onReclaim?.());
    panel.append(
      el("div", { class: "fd-takeover__head" }, [
        el("span", {
          class: "fd-takeover__title",
          text: takenOverTitle(state.takenOverBy),
        }),
        el("span", { class: "fd-takeover__eyebrow", text: "taken over" }),
      ]),
      el("p", { class: "fd-takeover__body", text: TAKEN_OVER_DETAIL }),
      el("p", {
        class: "fd-takeover__stale",
        text: "the terminal behind this panel is a photograph from the moment the worker was taken",
      }),
      el("div", { class: "fd-takeover__actions" }, [button]),
    );
  }

  return { el: layer, update: render };
}
