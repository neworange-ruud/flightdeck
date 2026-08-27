import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
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
 *
 * This function is scaffold: it proves xterm.js constructs, mounts and
 * accepts the host's geometry. The real main screen (artboards 1a-1c) that
 * streams actual PTY bytes and wires reconnect/resume is remote-control-sk4u.
 */
export function mountTerminal(
  container: HTMLElement,
  geometry: TerminalGeometry,
): Terminal {
  const term = new Terminal({
    cols: geometry.cols,
    rows: geometry.rows,
    // The desktop is the source of ground truth for content; a browser tab
    // simply displays it, so no local scrollback beyond what the replay ring
    // buffer (D2) already gives a reconnecting viewer.
    scrollback: 0,
    disableStdin: false,
    fontFamily: "var(--fd-font-mono)",
    fontSize: 13,
    // Tokens are the source of truth for colour (see src/style/tokens.css);
    // xterm.js needs literal hex here because its `theme` option cannot read
    // CSS custom properties. Kept minimal for the scaffold — the full ANSI
    // palette mapping is remote-control-sk4u's job.
    theme: {
      background: "#07111f", // --fd-ground
      foreground: "#dce7f7", // --fd-text-soft
      cursor: "#f5d76e", // --fd-focus
    },
  });

  term.open(container);
  return term;
}
