import type { SeatInfo } from "../state/model";
import { seatRoleLabel } from "../state/seats";
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
 *
 * ## The same panel, with rows (M3)
 *
 * 2f's caption ends "M3's multi-viewer list is the same panel with rows", and
 * that is meant literally: there is no second screen. The single incumbent's
 * `address / browser / connected` fact list widens into **one row per seat**,
 * which answers the multi-viewer question the fact list could not — *who else
 * is here* — without moving the answer to *who is typing* anywhere else.
 *
 * Three things the rows must keep straight, all of them consequences of D14's
 * revision rather than presentation choices:
 *
 * 1. **The role and the turn are two facts.** `seat` says whether a surface may
 *    type at all; `holdsInput` says whether it is typing this instant. Three
 *    writers with one mid-burst is the ordinary state, and it must be
 *    renderable — so the two are drawn as separate marks and never merged into
 *    a single "active" styling.
 * 2. **Which row is the reader's is the host's word** (`isYou`). Two tabs on
 *    one machine produce two identical-looking rows, and that is exactly the
 *    situation this panel exists for.
 * 3. **The rows come from `state.seats`, not from the takeover state.** That is
 *    what makes them live: a `Delta::Seats` while the panel is open repaints
 *    it, so a reader watching the lock move sees it move.
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
        /**
         * The roster when the host has sent one, and 2f's three rows about the
         * one writer that refused us when it has not.
         *
         * The fallback is not dead code: `WireError::seat_held` names the
         * holder and arrives before any dated seat list on a fresh socket, so
         * the panel can genuinely open with nothing but an incumbent. What the
         * fallback must not do is *look* like a roster — it describes one seat,
         * and drawing it as a one-row list would claim the reader is alone with
         * that writer.
         */
        state.seats.length === 0
          ? factList([
              ["address", incumbent.address],
              ["browser", incumbent.browser],
              ["connected", incumbent.connected],
            ])
          : seatList(state.seats),
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
        " is typing now, so yours stopped arriving mid-session",
        /**
         * 2f prints "the last one that landed was 3s ago", and the clause is
         * kept out entirely when we have no keystroke that landed to date it
         * from — a tab preempted before it ever typed. `just now` would be a
         * time we invented for an event that did not happen.
         */
        takeover.lastInputAgo === "" ? "" : "; the last one that landed was ",
        takeover.lastInputAgo === ""
          ? ""
          : el("span", {
              class: "fd-takeover__who",
              text: takeover.lastInputAgo,
            }),
        ". You are still seated — the input comes back the moment they pause.",
      ]),
      /** The same roster as the arriving panel: losing the turn is exactly when
       * "who else is here" becomes worth knowing, and it is the same panel. */
      state.seats.length === 0 ? "" : seatList(state.seats),
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

/**
 * Every seat the host has told us about, one row each, in the host's own order
 * (desktop first, then tabs by arrival).
 *
 * **The order is not re-sorted here, and in particular the reader's own row is
 * not floated to the top.** The host's order is stable for as long as a tab is
 * attached, and a list that reorders itself while somebody reads it is a list
 * that cannot be pointed at. `this tab` marks the row instead.
 *
 * Nothing is totalled. 2f is specific that seats are *named* rather than
 * counted, because the reason a second seat is not alarming is that you can see
 * it is your own desktop — and that reasoning gets stronger, not weaker, with
 * more rows.
 */
function seatList(seats: readonly SeatInfo[]): HTMLElement {
  const list = el("ul", {
    class: "fd-takeover__seats",
    attrs: { "aria-label": "Viewers" },
  });
  for (const seat of seats) {
    list.append(seatRow(seat));
  }
  return list;
}

/**
 * One seat: the turn, the name, whether it is the reader's, the role, and how
 * long it has been here.
 *
 * The turn and the role are two marks rather than one because they are two
 * facts (D14 as revised): `✎` — the chip's own glyph for it — is on the row
 * that is typing *now*, and it is empty on every other row including the
 * writers that may type and are not. Three writers with one mid-burst therefore
 * draws three rows saying `can type`/`typing now`, which is precisely what the
 * merged v1 flag could not express.
 *
 * A row whose `sinceLabel` is empty — a host that sent no clock to date its
 * rows against — drops the `connected` line rather than printing a fabricated
 * duration, the same rule `factList` applies below.
 */
function seatRow(seat: SeatInfo): HTMLElement {
  return el(
    "li",
    {
      class: "fd-takeover__seat",
      attrs: {
        "data-seat": seat.seat,
        "data-holds-input": String(seat.holdsInput),
        "data-you": String(seat.isYou),
      },
    },
    [
      el("span", {
        class: "fd-takeover__seat-mark",
        /** The chip's glyph for the turn, on the one row that has it. Empty
         * rather than a placeholder character on the others: the column is
         * held open by the grid, not by a glyph that means nothing. */
        text: seat.holdsInput ? "✎" : "",
        attrs: { "aria-hidden": "true" },
      }),
      el("span", { class: "fd-takeover__seat-label", text: seat.label }),
      /** `this tab` is the chip's own word for the reader's own seat, and it
       * comes from the host's `is_you` — never from matching an address. */
      seat.isYou
        ? el("span", { class: "fd-takeover__seat-you", text: "this tab" })
        : "",
      el("span", {
        class: "fd-takeover__seat-role",
        text: seatRoleLabel(seat),
      }),
      seat.sinceLabel === ""
        ? ""
        : el("span", {
            class: "fd-takeover__seat-since",
            text: `connected ${seat.sinceLabel}`,
          }),
    ],
  );
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
