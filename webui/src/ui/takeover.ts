import { webController } from "../state/seats";
import type { AppState } from "../state/types";
import { clear, el } from "./dom";
import type { Region } from "./dom";

/**
 * Artboard `2f — TAKEOVER`: the arriving browser, the evicted browser, and
 * read-only observation as a real destination from both.
 *
 * ## Two protocol facts this component is shaped by
 *
 * **Takeover has no dedicated frame.** There is no `ClientMsg::TakeOver` in
 * protocol v1 — the browser re-sends `Attach { seat: SeatRequest::TakeOver }`.
 * So `Take over` and `Take it back` are the same act from the wire's point of
 * view, and this component reports one intent for both.
 *
 * **Eviction is a `Delta::Seats`, never a `Shutdown`.** The evicted socket
 * stays open. That is what makes the evicted panel a prompt over a *live*
 * connection rather than a terminal state, and it is why `Watch read-only` is
 * an offer we can actually keep: the bytes are already arriving.
 *
 * ## Magenta, because nothing here is broken
 *
 * `--fd-elsewhere` is 2g's "another actor acted" hue, shared with drift and App
 * mode. D14 is explicit that this is **courtesy, not a permission check**:
 * anyone holding the credential can evict anyone. The panel exists so that
 * neither of the two people wonders why the keys stopped working — not to ask
 * for a right the protocol does not grant.
 */

export interface TakeoverOptions {
  /** `Enter` — re-`Attach` as the controller (`SeatRequest::TakeOver`). */
  readonly onClaim?: () => void;
  /** `w` — watch read-only (`SeatRequest::Observe`). */
  readonly onObserve?: () => void;
  /**
   * `Esc` — cancel. Offered on the arriving panel only, and it lands in the
   * same place as `w`: 2f is explicit that *"cancelling still leaves a live
   * read-only view"*, because observation costs the host nothing and answers
   * "is it done yet?" without evicting anyone.
   */
  readonly onCancel?: () => void;
}

export function createTakeover(options: TakeoverOptions = {}): Region {
  const panel = el("div", { class: "fd-takeover__panel" });
  const layer = el(
    "div",
    {
      class: "fd-takeover",
      attrs: { role: "dialog", "aria-label": "Browser control" },
    },
    [panel],
  );

  function render(state: AppState): void {
    const takeover = state.takeover;
    layer.hidden = takeover === null;
    if (takeover === null) {
      return;
    }
    layer.setAttribute("data-kind", takeover.kind);
    clear(panel);

    if (takeover.kind === "arriving") {
      const { incumbent } = takeover;
      panel.append(
        head("Someone else is driving", "arriving browser"),
        el("p", { class: "fd-takeover__body" }, [
          "Another browser is controlling this instance. FlightDeck accepts one browser at a time, so taking over will disconnect theirs.",
        ]),
        factList([
          ["address", incumbent.address],
          ["browser", incumbent.browser],
          ["connected", incumbent.connected],
        ]),
        el("p", {
          class: "fd-takeover__detail",
          /** 2f's own words, and the honest description of D14: not a lock. */
          text: "Probably you, on your phone. Anyone holding the code can do this — it is not a lock, just a warning so neither of you wonders why the keys stopped working.",
        }),
        actions([
          ["primary", "Enter", "Take over", options.onClaim],
          ["secondary", "w", "Watch read-only", options.onObserve],
          ["tertiary", "Esc", "Cancel", options.onCancel],
        ]),
      );
      return;
    }

    panel.append(
      head("Another browser took over", "evicted"),
      el("p", { class: "fd-takeover__body" }, [
        "A browser at ",
        el("span", { class: "fd-takeover__who", text: takeover.byAddress }),
        " is controlling FlightDeck now. Your keystrokes stopped arriving mid-session; the last one that landed was ",
        el("span", { class: "fd-takeover__who", text: takeover.lastInputAgo }),
        ".",
      ]),
      el("p", {
        class: "fd-takeover__stale",
        /** The amber note 2f puts inside the magenta panel — two different
         * facts (someone else is driving; what you see is old) that would be
         * confusing merged into one colour. */
        text: "the terminal behind this dialog is stale from the moment you lost control",
      }),
      el("p", {
        class: "fd-takeover__detail",
        text: "Reclaiming is one click and does the same thing to them. No trip to the desktop needed — you still hold the credential.",
      }),
      actions([
        ["primary", "Enter", "Take it back", options.onClaim],
        ["secondary", "w", "Watch read-only", options.onObserve],
      ]),
    );
  }

  return { el: layer, update: render };
}

/**
 * The incumbent as the seat list describes it, for a caller that has a
 * `Delta::Seats` but no `WireError::incumbent`.
 *
 * **This is the function that must not be written naively.** The desktop's row
 * is *always* `Seat::Controlling` — its keyboard is never revoked by a browser
 * taking over — so "find the controlling seat" finds two rows and would name
 * the desktop as the browser that evicted you. `webController` is the only
 * correct question: a viewer (`viewer_id: Some(_)`) *and* controlling.
 */
export function incumbentFromSeats(state: AppState): {
  readonly address: string;
  readonly browser: string;
  readonly connected: string;
} | null {
  const controller = webController(state.seats);
  if (controller === null) {
    return null;
  }
  return {
    /** The host sends one label per seat, rendered verbatim; splitting it into
     * address and browser here would be parsing untrusted display text. */
    address: controller.label,
    browser: "",
    connected: controller.sinceLabel,
  };
}

function head(title: string, eyebrow: string): HTMLElement {
  return el("div", { class: "fd-takeover__head" }, [
    el("span", { class: "fd-takeover__title", text: title }),
    el("span", { class: "fd-takeover__eyebrow", text: eyebrow }),
  ]);
}

function factList(facts: readonly (readonly [string, string])[]): HTMLElement {
  const list = el("dl", { class: "fd-takeover__facts" });
  for (const [name, value] of facts) {
    if (value === "") {
      continue;
    }
    list.append(
      el("dt", { text: name }),
      el("dd", { text: value }),
    );
  }
  return list;
}

function actions(
  rows: readonly (readonly [
    string,
    string,
    string,
    (() => void) | undefined,
  ])[],
): HTMLElement {
  const row = el("div", { class: "fd-takeover__actions" });
  for (const [rank, key, label, handler] of rows) {
    const button = el(
      "button",
      {
        class: `fd-takeover__action fd-takeover__action--${rank}`,
        attrs: { type: "button", "data-key": key },
      },
      [el("span", { class: "fd-key-box", text: key }), label],
    );
    button.addEventListener("click", () => handler?.());
    row.append(button);
  }
  return row;
}
