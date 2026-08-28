import type { AppState } from "../state/types";
import { clear, el } from "./dom";
import type { Region } from "./dom";

/**
 * Artboard `2f — TAKEOVER`: the writer that was refused, the writer that was
 * interrupted, and read-only observation as a real destination from both.
 *
 * ## Three protocol facts this component is shaped by
 *
 * **Takeover has no dedicated frame.** There is no `ClientMsg::TakeOver` — the
 * browser re-sends `Attach { seat: SeatRequest::TakeOver }`. So `Take over` and
 * `Take it back` are the same act from the wire's point of view, and this
 * component reports one intent for both.
 *
 * **What is being taken is the input lock, not the seat** (D14 as revised).
 * Several browsers may be seated as writers at once; at any instant one of them
 * holds the turn. Nobody is disconnected and nobody is demoted, so both panels
 * are prompts over a *live* connection with the reader's own seat intact —
 * which is why waiting is a real option here and not just a polite word for
 * giving up: the lock frees itself once the holder goes quiet.
 *
 * **The desktop is one of the writers.** It is refused on the same terms and
 * can appear in these panels by name. There is no surface with precedence, and
 * the copy must not imply one.
 *
 * ## Magenta, because nothing here is broken
 *
 * `--fd-elsewhere` is 2g's "another actor acted" hue, shared with drift and App
 * mode. D14 is explicit that this is **courtesy, not a permission check**:
 * anyone holding the credential can interrupt anyone. The panel exists so that
 * neither of the two people wonders why the keys stopped working — not to ask
 * for a right the protocol does not grant.
 */

export interface TakeoverOptions {
  /** `Enter` — re-`Attach` and take the input lock (`SeatRequest::TakeOver`). */
  readonly onClaim?: () => void;
  /** `w` — watch read-only (`SeatRequest::Observe`). */
  readonly onObserve?: () => void;
  /**
   * `Esc` — cancel. Offered on the refused panel only.
   *
   * 2f is explicit that *"cancelling still leaves a live view"*, and under D14
   * as revised it leaves a live **writing** view: the refusal cost the turn,
   * not the seat, so cancelling means *I will wait* and the lock comes back on
   * its own. That is why this is no longer the same intent as `w`.
   */
  readonly onCancel?: () => void;
}

/** Lives in `state/seats.ts` now, because the reducer needs it too — see the
 * note there on why "find the controlling seat" is no longer a question with an
 * answer. */
export { incumbentFromSeats } from "../state/seats";

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
        head("Someone else is typing", "refused"),
        el("p", { class: "fd-takeover__body" }, [
          "A terminal has one cursor, so your keystrokes were refused rather than mixed into theirs. You still hold a seat: the input frees itself once they stop typing, or you can take it now — which interrupts them mid-word.",
        ]),
        factList([
          ["address", incumbent.address],
          ["browser", incumbent.browser],
          ["connected", incumbent.connected],
        ]),
        el("p", {
          class: "fd-takeover__detail",
          /** 2f's own words, kept honest under the revision: it *is* a lock
           * now, but a soft one — nobody is being kept out, and nobody has a
           * privilege here, the desktop included. */
          text: "Probably you, on your phone. Anyone holding the code can do this, and the desktop plays by the same rule — the lock exists so two people cannot corrupt one line, not to decide who is in charge.",
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
      head("Someone took the input", "interrupted"),
      el("p", { class: "fd-takeover__body" }, [
        "",
        el("span", { class: "fd-takeover__who", text: takeover.byAddress }),
        " is typing now, so yours stopped arriving mid-session; the last one that landed was ",
        el("span", { class: "fd-takeover__who", text: takeover.lastInputAgo }),
        ". You are still seated — the input comes back the moment they pause.",
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
        text: "Taking it back is one click and does the same thing to them. No trip to the desktop needed — you still hold the credential, and the seat.",
      }),
      actions([
        ["primary", "Enter", "Take it back", options.onClaim],
        ["secondary", "w", "Watch read-only", options.onObserve],
      ]),
    );
  }

  return { el: layer, update: render };
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
