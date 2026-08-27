import { reduce } from "./state/reducer";
import { createInitialState } from "./state/types";
import { mountTerminal } from "./term/terminal";

/**
 * Scaffold entry point. There is no server connection here yet — that is
 * `src/web/server.rs` (a separate task) plus this app's own websocket client
 * (also not yet written). This file exists to prove three things wire
 * together: the reducer, the palette/font stylesheets, and an xterm.js
 * instance that respects D4 (host-owned, letterboxed, no `FitAddon`).
 *
 * remote-control-sk4u replaces this with the real main screen against
 * artboards 1a/1b/1c.
 */

let state = createInitialState();

const app = document.querySelector<HTMLDivElement>("#app");
if (!app) {
  throw new Error("#app mount point missing from index.html");
}

const status = document.createElement("div");
status.className = "fd-status-line";
status.style.padding = "8px 12px";
status.style.fontSize = "var(--fd-t-meta)";
status.style.color = "var(--fd-text-quiet)";
status.style.letterSpacing = ".08em";
status.style.textTransform = "uppercase";

const stage = document.createElement("div");
stage.className = "fd-term-stage";

const mount = document.createElement("div");
mount.className = "fd-term-mount";
stage.append(mount);

app.append(status, stage);

// D4: this 120x34 stands in for the host's real geometry until
// ServerMsg::Snapshot exists (D12). It is never fitted to the container.
state = reduce(state, {
  type: "geometry/set",
  geometry: { cols: 120, rows: 34 },
});

if (state.geometry) {
  mountTerminal(mount, state.geometry);
}

state = reduce(state, { type: "connection/changed", status: "connected" });

function renderStatus(): void {
  const geometry = state.geometry;
  const grid = geometry ? `${geometry.cols}×${geometry.rows}` : "—";
  status.textContent = `${state.connection} · ${grid} · host owns geometry`;
}

renderStatus();
