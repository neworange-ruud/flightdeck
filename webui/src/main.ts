import { fixtureSnapshot } from "./state/fixture";
import { fixtureTerminalBytes } from "./state/fixtureBytes";
import { createApp } from "./ui/app";
import { mountTerminal } from "./term/terminal";
import type { TerminalMount } from "./ui/terminalStage";

/**
 * Entry point for the main screen (artboards 1a/1b/1c).
 *
 * Everything below the fold is fixture-driven: there is no websocket in this
 * task. `remote-control-hgqy` replaces the three fixture lines at the bottom
 * with a socket that dispatches the same actions — `snapshot/received` from
 * `ServerMsg::Snapshot`, `connection/changed` from the transport — and swaps
 * the `mount` below for one that pipes `ServerMsg::Delta` into `term.write`.
 * The components never learn the difference.
 */

const root = document.querySelector<HTMLDivElement>("#app");
if (root === null) {
  throw new Error("#app mount point missing from index.html");
}

/**
 * D4: construct xterm with the host's grid and letterbox it. `FitAddon` is
 * absent by design — see `src/term/terminal.ts`.
 */
const mount: TerminalMount = (container, geometry, terminalId) => {
  const term = mountTerminal(container, geometry);
  term.write(fixtureTerminalBytes(terminalId));
  return () => term.dispose();
};

const app = createApp({
  mount,
  /**
   * D3: a selection made here is the whole instance's selection, the desktop
   * included. There is no socket yet, so this is where the `ClientMsg::Command`
   * frame goes — announced rather than silently dropped, so nobody mistakes the
   * fixture for a working remote control.
   */
  onDispatch: (action) => {
    if (action.type.startsWith("selection/")) {
      console.info(
        "[fixture] selection changed locally; ClientMsg::Command { select } goes here (D3)",
        action,
      );
    }
  },
});

root.append(app.el);

app.store.dispatch({ type: "snapshot/received", snapshot: fixtureSnapshot() });
app.store.dispatch({ type: "connection/changed", status: "connected" });
/** 1a is drawn in Terminal mode, so that is the state the fixture shows. */
app.store.dispatch({ type: "mode/set", mode: "terminal" });
