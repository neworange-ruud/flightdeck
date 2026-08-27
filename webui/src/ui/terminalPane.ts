import type { AppState } from "../state/types";
import { el } from "./dom";
import type { Region } from "./dom";
import { createTerminalStage } from "./terminalStage";
import type { TerminalMount } from "./terminalStage";

/**
 * Region 5 of 7 — the terminal viewport, in its two 1a/1b treatments.
 *
 * Terminal mode (1a): full colour, the pane carries the focus glow, nothing
 * below it.
 *
 * App mode (1b): the terminal is **asleep**, not stale — a distinction 2d makes
 * carefully and this app must not blur. Asleep means "your keystrokes are going
 * somewhere else"; stale means "this picture is a photograph and nothing you
 * type is arriving". Asleep desaturates cool and drops bold
 * (`--fd-term-asleep`, lifted to 5.6:1 exactly because a whole screen of text
 * was the palette's least defensible dim value) and adds the footer strip that
 * says how to wake it. Stale — amber cast, scanlines, frozen clock, caret gone
 * — belongs to the connection-state work (`remote-control-l7ya`, artboards
 * 2c/2d); the `data-tone` attribute is where it plugs in.
 */
export function createTerminalPane(mount: TerminalMount): Region {
  const stage = createTerminalStage({
    terminalId: (state) => state.selection?.terminalId ?? null,
    mount,
    label: "terminal",
  });

  const foot = el("div", { class: "fd-pane__foot" }, [
    "terminal asleep — keystrokes go to FlightDeck · ",
    el("span", { class: "fd-key", text: "Enter" }),
    " or click to wake it",
  ]);

  const pane = el(
    "div",
    { class: "fd-pane", attrs: { "data-tone": "live" } },
    [stage.el, foot],
  );

  return {
    el: pane,
    update(state: AppState) {
      pane.setAttribute("data-tone", state.mode === "app" ? "asleep" : "live");
      stage.update(state);
    },
  };
}
