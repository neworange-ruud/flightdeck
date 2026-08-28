/**
 * The one decision behind the narrow layout: is this viewport below 900px?
 *
 * `remote-control-eek.4`, `specs/WEB_INTERFACE.md` §6.5 **R17**.
 *
 * ## Why this is a function and not a media query
 *
 * Artboard 1h states the position — *"Below 900px the sidebar becomes a
 * slide-over invoked from a session chip in the project row, and the git bar
 * folds into the status bar"* — but never draws it, and design turn 3 was not
 * run. So every consequence of that sentence had to be derived, and the one
 * thing that makes a derivation checkable is that somebody can *test* it.
 *
 * A `@media (max-width: …)` rule is untestable in this repository: `vitest`
 * runs in jsdom, which parses media queries and never evaluates them, so a
 * layout that lived in one would be asserted by nothing until the Playwright
 * job ran — and R6 registered that job **non-blocking until 2026-09-10**. The
 * same argument `tokens.guard.test.ts` makes for its four rules applies here:
 * a rule nothing checks in `npm run test` is a rule that drifts.
 *
 * So the decision is this pure function, the pixel width arrives on an action
 * exactly as `input/esc`'s timestamp does (the impurity is the caller's, the
 * decision is the reducer's), and CSS keys off `data-width` on `.fd-frame` —
 * which is the idiom the frame already uses for `data-mode`, `data-layout`,
 * `data-access`, `data-takeover`, `data-feed`, `data-dialog` and
 * `data-readonly`. The narrow layout is the eighth member of a family, not a
 * second stylesheet.
 *
 * `tokens.guard.test.ts` rule 5 keeps it that way: no width media query
 * anywhere in `src/style/`, and nothing under `state/` or `ui/` may measure
 * the window or an element.
 */

/**
 * 1h's number, verbatim: *"Below 900px"*. 900 itself is therefore **wide** —
 * "below" excludes the boundary, and a viewport exactly 900px across still has
 * room for 1a's 300px sidebar beside a terminal.
 */
export const NARROW_BELOW_PX = 900;

/**
 * Which of the two layouts a viewport gets. Two values, not a scale: 1h names
 * one breakpoint and this task refuses to invent a second (see R17 — a design
 * turn is free to add one, and it would be the turn's to draw).
 */
export type ViewportWidth = "wide" | "narrow";

/**
 * The whole rule. Guards against a nonsense measurement — a hidden iframe, a
 * jsdom default nobody set — by treating anything non-finite as **wide**,
 * because wide is the layout every artboard actually draws and falling back to
 * a layout nobody drew would be the worse failure.
 */
export function widthClass(pixels: number): ViewportWidth {
  if (!Number.isFinite(pixels) || pixels <= 0) {
    return "wide";
  }
  return pixels < NARROW_BELOW_PX ? "narrow" : "wide";
}
