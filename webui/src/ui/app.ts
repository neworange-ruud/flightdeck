import { createInitialState } from "../state/types";
import type { AppAction, AppState } from "../state/types";
import { createGitBar } from "./gitBar";
import { createLogoBand } from "./logoBand";
import { createProjectTabs } from "./projectTabs";
import { createSidebar } from "./sidebar";
import { createSplitView } from "./splitView";
import { createStatusBar } from "./statusBar";
import { createStore } from "./store";
import { createTerminalPane } from "./terminalPane";
import { createTerminalTabs } from "./terminalTabs";
import { el } from "./dom";
import type { Region } from "./dom";
import type { Store } from "./store";
import type { TerminalMount } from "./terminalStage";

/**
 * The main screen: artboards 1a (Terminal mode), 1b (App mode) and 1c (split).
 *
 * Assembly only — every pixel decision lives in the seven region modules and in
 * `src/style/main.css`. What this file owns is the two things that are neither:
 * the mode/layout attribute that switches 1a into 1b, and the keyboard and
 * pointer rules the design locked in (spec §5).
 *
 * ## How the other two tasks plug in
 *
 * `remote-control-hgqy` (live socket) replaces two arguments and nothing else:
 *   - `mount`, so a new terminal is also subscribed to `ServerMsg::Delta`;
 *   - `onDispatch`, so `selection/*` becomes a `ClientMsg::Command` and
 *     `input/*` becomes `ClientMsg::Input`.
 * It then dispatches `snapshot/received` from `ServerMsg::Snapshot` instead of
 * from the fixture, and `connection/changed` from the transport. No component
 * changes, because no component reads anything but `AppState`.
 *
 * `remote-control-l7ya` (access screens, connection states, takeover, activity
 * feed) adds *siblings*, not edits: a screen chooser above `createApp` for the
 * access/pairing states, `data-tone="stale"` on the pane for 2c/2d, and a feed
 * slide-over appended to the frame. The connection-dependent chrome it needs
 * already exists — `modeChip` drains on any non-connected state (§5.1) and
 * `connectionLabel` holds the two held-input phrases.
 */

export interface AppOptions {
  /** How a terminal gets into the DOM. Injected so tests need no canvas. */
  readonly mount: TerminalMount;
  /** Every action, after reduction — the seam for `ClientMsg` frames (D3). */
  readonly onDispatch?: (action: AppAction, state: AppState) => void;
  /** Clock for the `Esc Esc` window; overridden in tests. */
  readonly now?: () => number;
}

export interface App {
  readonly el: HTMLElement;
  readonly store: Store;
}

export function createApp(options: AppOptions): App {
  const now = options.now ?? (() => performance.now());
  const store = createStore(
    createInitialState(),
    options.onDispatch === undefined
      ? {}
      : { onDispatch: options.onDispatch },
  );

  const logo = createLogoBand();
  const projects = createProjectTabs(store);
  const sidebar = createSidebar(store);
  const tabs = createTerminalTabs(store);
  const pane = createTerminalPane(options.mount);
  const gitBar = createGitBar();
  const statusBar = createStatusBar();

  const main = el(
    "div",
    { class: "fd-main", attrs: { "aria-label": "Terminal" } },
    [tabs.el, pane.el],
  );
  const body = el("div", { class: "fd-body" }, [sidebar.el, main]);
  const frame = el(
    "div",
    {
      class: "fd-frame",
      attrs: { "data-mode": "app", "data-layout": "single" },
    },
    [logo.el, projects.el, body, gitBar.el, statusBar.el],
  );

  /** 1c is built the first time it is needed: three xterm instances nobody can
   * reach yet (split toggling is M2, D8) would be three wasted PTY subscriptions. */
  let split: Region | null = null;

  const regions: readonly Region[] = [
    logo,
    projects,
    sidebar,
    tabs,
    pane,
    gitBar,
    statusBar,
  ];

  function render(state: AppState): void {
    frame.setAttribute("data-mode", state.mode);
    frame.setAttribute("data-layout", state.layout);

    if (state.layout === "split") {
      if (split === null) {
        split = createSplitView(store, options.mount);
        main.append(split.el);
      }
      tabs.el.hidden = true;
      pane.el.hidden = true;
      split.el.hidden = false;
    } else {
      tabs.el.hidden = false;
      pane.el.hidden = false;
      if (split !== null) {
        split.el.hidden = true;
      }
    }

    for (const region of regions) {
      region.update(state);
    }
    if (split !== null && state.layout === "split") {
      split.update(state);
    }
  }

  store.subscribe(render);
  render(store.getState());

  /**
   * §5, the keyboard positions the design locked in:
   *
   *   - `Ctrl-g` is the **only** chord the app claims. It is swallowed here so
   *     the browser's own Ctrl-g never fires on a FlightDeck screen; what it
   *     opens (the command palette) is M2 by D8, so today it opens nothing.
   *   - `Esc Esc` within 400 ms leaves terminal focus, and a **single `Esc`
   *     still passes through to the agent** — `esc to interrupt` is the key
   *     users press most, so the app refuses to eat it. The timing lives in
   *     `decideEscape`; the reducer applies it.
   *   - `Enter` in App mode focuses the terminal (1b's own footer says so).
   *
   * When `remote-control-hgqy` wires input, keystrokes will arrive through
   * xterm's `onData`, and the `Esc` branch below must move to
   * `attachCustomKeyEventHandler` so it stays the single authority — otherwise
   * an `Esc` would be both queued here and sent there.
   */
  frame.addEventListener("keydown", (event: KeyboardEvent) => {
    const state = store.getState();

    if (event.key === "g" && event.ctrlKey) {
      event.preventDefault();
      return;
    }

    if (event.key === "Escape" && state.mode === "terminal") {
      event.preventDefault();
      store.dispatch({ type: "input/esc", at: now() });
      return;
    }

    if (event.key === "Enter" && state.mode === "app") {
      event.preventDefault();
      store.dispatch({ type: "mode/set", mode: "terminal" });
    }
  });

  /**
   * The pointer half of the same rule: clicking the terminal wakes it, and
   * **clicking outside releases the keys** — the escape hatch for a user who
   * does not want to learn a chord, and the reason 1a's status bar advertises
   * `click outside release keys`.
   */
  frame.addEventListener("click", (event: MouseEvent) => {
    const target = event.target;
    if (!(target instanceof Node)) {
      return;
    }
    const inTerminal =
      pane.el.contains(target) || (split?.el.contains(target) ?? false);
    const mode = store.getState().mode;
    if (inTerminal && mode !== "terminal") {
      store.dispatch({ type: "mode/set", mode: "terminal" });
    } else if (!inTerminal && mode === "terminal") {
      store.dispatch({ type: "mode/set", mode: "app" });
    }
  });

  return { el: frame, store };
}
