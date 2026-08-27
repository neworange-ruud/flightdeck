/**
 * Read a design token at runtime.
 *
 * This exists for exactly one caller: xterm.js. Its `theme` and `fontSize`
 * options are JavaScript values, not CSS declarations, so they cannot be given
 * `var(--fd-ok)` — and hard-coding the hex values there would put twenty colour
 * literals into the app, which is the one thing `tokens.css` exists to prevent.
 * Reading the computed value off `:root` keeps `tokens.css` the single source of
 * truth even for the terminal's ANSI palette.
 *
 * Returns `null` rather than a fallback colour when the token is missing or the
 * environment has no layout engine (a `node` test run). Callers then omit the
 * key and let xterm use its own default, which is honest: an app that invented
 * a colour here would be shipping an untracked twenty-first literal.
 */
export function readToken(name: string): string | null {
  if (typeof document === "undefined" || typeof getComputedStyle !== "function") {
    return null;
  }
  const value = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
  return value === "" ? null : value;
}

/** Read a token that holds a length in `px` and return the number. */
export function readTokenPx(name: string): number | null {
  const raw = readToken(name);
  if (raw === null) {
    return null;
  }
  const parsed = Number.parseFloat(raw);
  return Number.isFinite(parsed) ? parsed : null;
}
