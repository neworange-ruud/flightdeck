import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { readToken, readTokenPx } from "../style/tokens";
import type { ITheme } from "@xterm/xterm";
import type { TerminalGeometry } from "../state/types";

/**
 * Construct the xterm.js instance and mount it into `container`.
 *
 * D4 (as revised by turn 2) is the whole reason this function looks the way
 * it does: the desktop TUI owns PTY geometry, full stop. `sync_selected_tab_
 * sizes` (`src/lib.rs:5389`) calls `resize_if_changed` every frame for the
 * selected tab, so anything the browser did to claim its own size would be
 * reverted within one frame. The browser's job is to LETTERBOX the host's
 * fixed grid at its natural pixel size, centred on the terminal ground, with
 * leftover space left dark — never to scale or refit it.
 *
 * Concretely that means:
 *   - `cols`/`rows` are passed in from the host (eventually via
 *     `ServerMsg::Snapshot`/`Delta`, D12) and used verbatim.
 *   - `@xterm/addon-fit`'s `FitAddon` MUST NEVER be imported anywhere in this
 *     app. Adding it back is the single most likely accidental regression of
 *     D4 turn 2 — it exists precisely to make a terminal claim its
 *     container's size, which is the one thing this screen must not do.
 *   - Resizing the *container* (window resize, sidebar toggle) must never
 *     call `terminal.resize()`. Only a new `geometry` from the host may.
 */
export function mountTerminal(
  container: HTMLElement,
  geometry: TerminalGeometry,
): Terminal {
  // 2g: four type sizes, and the terminal body is `--fd-t-body`. Read from the
  // stylesheet rather than written here, because a number typed into this file
  // is a fifth size nobody would ever find again. Spread conditionally so a
  // missing token leaves xterm on its own default instead of `undefined`.
  const fontSize = readTokenPx("--fd-t-body");

  const term = new Terminal({
    cols: geometry.cols,
    rows: geometry.rows,
    // The desktop is the source of ground truth for content; a browser tab
    // simply displays it, so no local scrollback beyond what the replay ring
    // buffer (D2) already gives a reconnecting viewer.
    scrollback: 0,
    disableStdin: false,
    fontFamily: "var(--fd-font-mono)",
    ...(fontSize !== null ? { fontSize } : {}),
    theme: themeFromTokens(),
  });

  term.open(container);
  return term;
}

/**
 * The ANSI palette, mapped onto the semantic palette.
 *
 * xterm's `theme` is a JS object and cannot hold `var(--fd-*)`, so the values
 * are read out of `tokens.css` at runtime (`src/style/tokens.ts`) instead of
 * being duplicated as hex here — the same rule every component is under. The
 * *mapping* is the design decision worth reading: ANSI red is the app's alert
 * hue, ANSI yellow is the focus/selection hue, ANSI cyan is the interactive
 * hue, and bright black is the decoration tier, so an agent's own colour
 * choices land inside the palette instead of beside it.
 *
 * `--fd-stale` has no ANSI slot on purpose: amber means "this whole surface is
 * a photograph" (2d) and must never be reachable by a byte the agent emitted.
 */
function themeFromTokens(): ITheme {
  const theme: Record<string, string> = {};
  const put = (key: string, token: string): void => {
    const value = readToken(token);
    if (value !== null) {
      theme[key] = value;
    }
  };

  put("background", "--fd-ground");
  put("foreground", "--fd-text-soft");
  put("cursor", "--fd-focus");
  put("cursorAccent", "--fd-ground");
  put("selectionBackground", "--fd-tui-select");

  put("black", "--fd-canvas");
  put("red", "--fd-alert");
  put("green", "--fd-ok");
  put("yellow", "--fd-focus");
  put("blue", "--fd-info");
  put("magenta", "--fd-elsewhere");
  put("cyan", "--fd-accent");
  put("white", "--fd-text-muted");

  put("brightBlack", "--fd-text-decor");
  put("brightRed", "--fd-alert");
  put("brightGreen", "--fd-ok");
  put("brightYellow", "--fd-focus");
  put("brightBlue", "--fd-info");
  put("brightMagenta", "--fd-elsewhere");
  put("brightCyan", "--fd-accent");
  put("brightWhite", "--fd-text");

  return theme as ITheme;
}
