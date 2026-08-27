/**
 * The one keyboard timing rule the design locked in (spec §5, artboard 1h):
 *
 *   palette-primary, with `Ctrl-g` as the only chord the app claims;
 *   **`Esc Esc` within 400 ms — or a click outside — leaves terminal focus,
 *   while a single `Esc` still passes through to the agent.**
 *
 * That single-`Esc`-passes-through half is the load-bearing part: `esc to
 * interrupt` is how you stop a Claude Code turn, so a web surface that ate
 * `Esc` to open its own UI would take away the one key users press most. The
 * cost accepted is that leaving focus always costs the agent one harmless
 * `Esc`; the design chose that over stealing the key.
 *
 * It lives here, as a pure function of (armed-at, now), for three reasons:
 * it is the only piece of this screen with a *time* in it; a timer inside a
 * component would be untestable without fake clocks; and the reducer must stay
 * pure, so the action carries the timestamp instead of the reducer reading a
 * clock.
 *
 * The command palette itself is M2 (`Ctrl-g`, D8 puts it out of M1) — this
 * module decides *whether focus is released*, not what opens next.
 */

/** §5: the window is 400 ms, not a feel-it-out value. */
export const ESC_ESC_WINDOW_MS = 400;

export type EscapeOutcome =
  /**
   * Send the `Esc` to the agent and arm the window. `armedAt` is the timestamp
   * the caller must remember; a second `Esc` inside the window then releases
   * focus.
   */
  | { readonly kind: "pass_through"; readonly armedAt: number }
  /** Leave terminal focus (App mode). The first `Esc` was already delivered. */
  | { readonly kind: "leave_focus" };

/**
 * @param armedAt when the previous `Esc` was seen, or `null` if there was none
 *                (or the window already lapsed and was cleared)
 * @param at      when this `Esc` was seen
 * @param windowMs override only in tests
 *
 * A gap of exactly `windowMs` counts as *inside* the window: the design states
 * "within 400ms", and a user who hits the boundary exactly meant the chord.
 * Non-monotonic clocks (`at < armedAt`, which a wall-clock jump can produce)
 * are treated as a fresh press rather than a chord, because a negative gap is
 * not evidence of intent.
 */
export function decideEscape(
  armedAt: number | null,
  at: number,
  windowMs: number = ESC_ESC_WINDOW_MS,
): EscapeOutcome {
  if (armedAt !== null) {
    const gap = at - armedAt;
    if (gap >= 0 && gap <= windowMs) {
      return { kind: "leave_focus" };
    }
  }
  return { kind: "pass_through", armedAt: at };
}
