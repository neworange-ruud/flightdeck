import { el } from "./dom";
import type { Region } from "./dom";

/**
 * Region 1 of 7 — the logo band (1a, top strip).
 *
 * The two ramps are pure decoration (delete them and no fact is lost), so they
 * are `aria-hidden`; a screen reader that read "block block block medium shade"
 * twice per page would be worse than silence.
 *
 * The band's gradient is the one place mode shows up above the fold: 1b tints
 * its edges with `--fd-elsewhere`, the "another actor has the keyboard" hue.
 * That is a CSS rule keyed off `data-mode` on the frame, so this component has
 * nothing to update.
 */
export function createLogoBand(): Region {
  const band = el(
    "header",
    { class: "fd-logo", attrs: { "aria-label": "FlightDeck" } },
    [
      el("span", {
        class: "fd-logo__ramp",
        text: "████▓▓▓▒▒▒░░░",
        attrs: { "aria-hidden": "true" },
      }),
      el("span", { class: "fd-logo__word", text: "· FLIGHTDECK ·" }),
      el("span", {
        class: "fd-logo__ramp",
        text: "░░░▒▒▒▓▓▓████",
        attrs: { "aria-hidden": "true" },
      }),
    ],
  );

  return {
    el: band,
    update() {
      /* static */
    },
  };
}
