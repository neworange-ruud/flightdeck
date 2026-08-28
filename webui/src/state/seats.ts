import type { Incumbent, SeatInfo } from "./model";

/**
 * The viewer chip (2c/2f), and the seat questions that have wrong answers.
 *
 * ## A seat is a role; the input lock is a turn
 *
 * D14 as revised gives out as many **writer** seats as ask for one, and
 * arbitrates the scarce thing separately: at any instant at most one writer
 * holds the **input lock** and may actually type. The desktop is one of those
 * writers and gets no precedence — it contends and is refused on exactly the
 * same terms a browser is.
 *
 * Two consequences shape every function here:
 *
 * 1. **"Who is controlling" is not a question with an answer any more.** There
 *    is "who may type" (`writers`) and "who is typing" (`inputHolder`), and
 *    conflating them is how the old `webController` came to report the desktop
 *    as the browser that evicted you.
 * 2. **The desktop's row is always `writing`.** Its keyboard is never revoked,
 *    so filtering on the role alone always finds it. Anything that means "the
 *    other browser" must say `!seat.isDesktop` as well.
 */

/**
 * The writer that holds the input lock, or `null` when it is free.
 *
 * Free is a real and common state: nobody has typed for `INPUT_LOCK_IDLE_MS`,
 * so the next surface to type takes it. `null` is therefore *not* an error and
 * *not* "the desktop has it" — inventing a holder is exactly the asymmetry the
 * arbitration model refuses.
 */
export function inputHolder(seats: readonly SeatInfo[]): SeatInfo | null {
  return seats.find((seat) => seat.holdsInput) ?? null;
}

/** Every surface that may contend for input, the desktop included. */
export function writers(seats: readonly SeatInfo[]): readonly SeatInfo[] {
  return seats.filter((seat) => seat.seat === "writing");
}

/** Every browser that is watching without input (D14's fan-out, unchanged). */
export function observers(seats: readonly SeatInfo[]): readonly SeatInfo[] {
  return seats.filter((seat) => !seat.isDesktop && seat.seat === "observing");
}

/**
 * The chip's text: the seats that can type, named, with the one that is typing
 * marked.
 *
 * 2f is specific about the form — **named seats, not a counter that implies a
 * crowd** — so this joins labels and never totals them. What the revision adds
 * is the second fact: `desktop ✎ + this tab` says who is typing *now*, which is
 * the only honest answer to "why did my keys stop working" and is exactly what
 * v1's `desktop + this tab` could not say.
 *
 * Observers are counted rather than named, because the list must stay one line:
 * they cannot type, so who exactly they are is the viewer panel's business
 * (`remote-control-eek.3`), not the chip's.
 *
 * `fallbackCount` is used only when the host sent no seat list at all; saying
 * "2 viewers" is worse than naming them, and better than saying nothing.
 */
export function viewerChipText(
  seats: readonly SeatInfo[],
  fallbackCount: number,
): string {
  if (seats.length === 0) {
    return `${fallbackCount} viewer${fallbackCount === 1 ? "" : "s"}`;
  }
  const named = writers(seats)
    .map((seat) => (seat.holdsInput ? `${seat.label} ✎` : seat.label))
    .join(" + ");
  const watching = observers(seats).length;
  if (watching === 0) {
    return named;
  }
  return `${named} + ${watching} watching`;
}

/**
 * The chip's tooltip: every seat with what it is allowed to do, whether it is
 * doing it, and how long it has been there. The detail 2f calls fair — enough
 * for the humans to work out who is who, and no more.
 */
export function viewerChipTitle(seats: readonly SeatInfo[]): string {
  if (seats.length === 0) {
    return "the host sent no seat list";
  }
  return seats
    .map((seat) => {
      const role =
        seat.seat === "observing"
          ? "read-only"
          : seat.holdsInput
            ? "typing now"
            : "can type";
      const since = seat.sinceLabel === "" ? "" : ` · ${seat.sinceLabel}`;
      return `${seat.label} — ${role}${since}`;
    })
    .join("\n");
}

/**
 * The lock's holder as the seat list describes it — 2f's three rows, each from
 * its own field.
 *
 * **This is the function that must not be written naively.** Under v1 it looked
 * for the controlling seat and found two rows, because the desktop's is always
 * controlling. Under the revision the question is different and the trap is
 * gone: `holdsInput` is true of at most one row in the whole list, desktop
 * included, so there is nothing to disambiguate — and the desktop *can* be the
 * answer, which is the point of a symmetric rule.
 *
 * The label is *not* split: a user-agent string is untrusted free text that can
 * contain the separator, so the host keeps the facts apart on the wire instead
 * (`SeatInfo::address`, `SeatInfo::user_agent_label`). A host that sends neither
 * leaves us with the merged label and nothing to say about the browser — so the
 * label goes in the slot it is a true answer for (it starts with the address),
 * and the browser row is dropped rather than filled with half of a string we
 * refused to parse.
 *
 * It lives here rather than beside the panel because the reducer uses it too:
 * `WireError::seat_held` names the holder before any dated seat list has
 * arrived, so the panel opens with `connected` blank and is completed the moment
 * one does. See `refreshArrivingIncumbent` in `state/reducer.ts`.
 */
export function incumbentFromSeats(
  seats: readonly SeatInfo[],
): Incumbent | null {
  const holder = inputHolder(seats);
  if (holder === null) {
    return null;
  }
  return {
    address: holder.address ?? holder.label,
    browser: holder.browser ?? "",
    connected: holder.sinceLabel,
  };
}
