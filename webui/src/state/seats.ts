import type { SeatInfo } from "./model";

/**
 * The viewer chip (2c/2f) and the one seat question that has a wrong answer.
 *
 * **The desktop's row is always `Seat::Controlling`.** Its keyboard is never
 * revoked by a browser taking over — the desktop does not lose control, which
 * is exactly why 2f gives it a transient strip rather than a dialog. So a
 * "find the controlling seat" that only looks at `seat` finds **two** rows on a
 * perfectly ordinary connection, and would report the desktop as the browser
 * that evicted you.
 *
 * The single *web* controller is the row that is both a viewer
 * (`viewer_id: Some(_)`, mirrored here as `isDesktop: false`) **and**
 * controlling. That is what `webController` returns, and it is the only way
 * anything in this app should ask.
 */

/**
 * The one browser that holds input, or `null` when the seat is free — a desktop
 * plus N observers is a legal and common state.
 */
export function webController(
  seats: readonly SeatInfo[],
): SeatInfo | null {
  return (
    seats.find((seat) => !seat.isDesktop && seat.seat === "controlling") ?? null
  );
}

/** Every browser that is watching without input (D14's fan-out). */
export function observers(seats: readonly SeatInfo[]): readonly SeatInfo[] {
  return seats.filter((seat) => !seat.isDesktop && seat.seat === "observing");
}

/**
 * The chip's text: `desktop + this tab`.
 *
 * 2f is specific about the form — **two named seats, not a counter that implies
 * a crowd** — so this joins labels and never totals them. M3's multi-viewer
 * list is "the same panel with rows", so the labels are what will grow, not a
 * number.
 *
 * `fallbackCount` is used only when the host sent no seat list at all (an older
 * host, or a snapshot from before `Delta::Seats`); saying "2 viewers" is worse
 * than naming them, and better than saying nothing.
 */
export function viewerChipText(
  seats: readonly SeatInfo[],
  fallbackCount: number,
): string {
  if (seats.length === 0) {
    return `${fallbackCount} viewer${fallbackCount === 1 ? "" : "s"}`;
  }
  return seats.map((seat) => seat.label).join(" + ");
}

/**
 * The chip's tooltip: every seat with what it is allowed to do and how long it
 * has been there. The detail 2f calls fair — enough for the two humans to work
 * out who is who, and no more.
 */
export function viewerChipTitle(seats: readonly SeatInfo[]): string {
  if (seats.length === 0) {
    return "the host sent no seat list";
  }
  return seats
    .map((seat) => {
      const role = seat.seat === "controlling" ? "controls input" : "read-only";
      const since = seat.sinceLabel === "" ? "" : ` · ${seat.sinceLabel}`;
      return `${seat.label} — ${role}${since}`;
    })
    .join("\n");
}
