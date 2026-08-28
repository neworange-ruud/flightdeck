import { accessCopy, canSubmit } from "../state/access";
import { decidingKeys, hasToggle, primaryKey } from "../state/dialog";
import { highlightedCommand } from "../state/commands";
import type { PaletteCommand } from "../state/commands";
import type { ConfigSaveRequest } from "../state/config";
import { createInitialState } from "../state/types";
import type { AppAction, AppState } from "../state/types";
import type { StripAction } from "../state/connection";
import type { ActivityEvent } from "../state/model";
import { createAccessScreen } from "./accessScreen";
import { createActivityFeed } from "./activityFeed";
import { createCommandPalette } from "./commandPalette";
import { createConfigManager } from "./configManager";
import { createDialog } from "./dialog";
import { createGitBar } from "./gitBar";
import { createLogoBand } from "./logoBand";
import { createProjectTabs } from "./projectTabs";
import { createSidebar } from "./sidebar";
import { createSplitView } from "./splitView";
import { createStatusBar } from "./statusBar";
import { createStore } from "./store";
import { createTakeover } from "./takeover";
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
 * feed) landed as three overlay siblings appended to this frame — the access
 * layer (2b), the takeover layer (2f) and the feed slide-over (2e) — plus the
 * keyboard rules below. One deliberate deviation from the plan it was handed:
 * the access screens are a **layer inside this frame**, not a screen chosen
 * above `createApp`, because 2b draws all three panels inside the app frame
 * (logo band above, footer strip below, and a running agent visible behind the
 * revoked one — which 2b describes in words as "a photograph from the moment
 * access ended"). See `src/ui/accessScreen.ts`.
 *
 * ## What still has to move when the socket lands
 *
 * Besides the `Esc` handler below, `remote-control-hgqy` owns four dispatches
 * this file only *reports* the intent of, via `onDispatch`:
 *
 * | Intent reported here | Frame it becomes |
 * | --- | --- |
 * | `takeover/claim` | re-`Attach { seat: SeatRequest::TakeOver }` (there is no takeover frame) |
 * | `takeover/observe` | re-`Attach { seat: SeatRequest::Observe }` |
 * | `selection/jump` | `ClientMsg::Command { select_session }` (D3) |
 * | a `retry`/`reload`/`code` strip action | reconnect, `location.reload()`, or the access screen |
 */

export interface AppOptions {
  /** How a terminal gets into the DOM. Injected so tests need no canvas. */
  readonly mount: TerminalMount;
  /** Every action, after reduction — the seam for `ClientMsg` frames (D3). */
  readonly onDispatch?: (action: AppAction, state: AppState) => void;
  /** Clock for the `Esc Esc` window; overridden in tests. */
  readonly now?: () => number;
  /**
   * 2b's `Enter Connect`: exchange the four digits for a cookie
   * (`POST /auth/exchange`). Injected because it is the one thing on this
   * screen that talks to the network, and no test should.
   */
  readonly onSubmitCode?: (code: string) => void;
  /**
   * 2c's one keyed button per state — `r Retry now`, `Enter Reload for
   * v1.17.0`. Reconnecting and reloading are both outside this component's
   * remit: one belongs to the transport, the other to the page.
   */
  readonly onStripAction?: (action: StripAction) => void;
  /**
   * `Enter`, or a click on a row, inside the command palette (1d,
   * `remote-control-ll5.2`). Sending the frame and finding out what happened
   * to it are both outside this component's remit — `main.ts` calls
   * `SessionSocket.sendCommand`, gets back the seq the transport assigned,
   * and dispatches `palette/dispatched` with it, exactly as `onSubmitCode`
   * owns the one network call 2b needs.
   */
  readonly onRunCommand?: (command: PaletteCommand) => void;
  /**
   * `s` inside the configuration manager (1f, `remote-control-ll5.6`).
   * Sending the frame and finding out what happened to it are both outside
   * this component's remit, same as `onRunCommand` — `main.ts` calls
   * `SessionSocket.sendCommand(SAVE_CONFIG_COMMAND, request)`, gets back the
   * seq the transport assigned, and dispatches `config/dispatched` with it.
   */
  readonly onSaveConfig?: (request: ConfigSaveRequest) => void;
  /**
   * D13's shared dialog (1d/1e, `remote-control-ll5.3`). `key` is the button
   * pressed; `null` means cancel.
   *
   * Same seam as `onRunCommand`: this component reports the intent, `main.ts`
   * sends `dialog_confirm` / `dialog_cancel` and dispatches
   * `dialog/dispatched` with the seq the transport assigned. What it must
   * **not** do is close the dialog — a dialog is app state, and only a
   * `Delta::DialogClosed` from the host takes it off either surface.
   */
  readonly onAnswerDialog?: (key: string | null) => void;
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

  /** 2e's slide-over lives inside the terminal area, at its right edge. */
  const feed = createActivityFeed({
    onJump: (event: ActivityEvent) => jumpTo(event),
    onClose: () => store.dispatch({ type: "feed/set", open: false }),
  });
  const takeover = createTakeover({
    onClaim: () => store.dispatch({ type: "takeover/claim" }),
    onObserve: () => store.dispatch({ type: "takeover/observe" }),
    /** D14/2f: cancelling is not a dead end — it leaves a live read-only view,
     * which is why it dispatches the same action `w` does. */
    onCancel: () => store.dispatch({ type: "takeover/observe" }),
  });
  const access = createAccessScreen({
    onSubmit: () => submitCode(),
    onDigit: (digit) => store.dispatch({ type: "access/digit", digit }),
    onRetry: () => store.dispatch({ type: "access/retry" }),
    onDismiss: () => store.dispatch({ type: "access/dismiss" }),
  });
  /** Artboard 1d, `remote-control-ll5.2`. Opened by `Ctrl-g` below — the only
   * chord the app claims (§5) — and closed by `Esc` or `Ctrl-g` again. */
  const palette = createCommandPalette({
    onRun: (command) => runCommand(command),
  });
  /** Artboard 1f, `remote-control-ll5.6`. Opened by the palette's "Open
   * Configuration" row (see `runCommand` below), closed by `Esc`. */
  const config = createConfigManager(store);
  /**
   * Artboards 1d/1e, `remote-control-ll5.3`. Unlike every other overlay here,
   * nothing local opens or closes it: it is on screen because the host
   * published a dialog (D13), and it goes away when the host closes it.
   */
  const dialog = createDialog(
    {
      onConfirm: (key) => options.onAnswerDialog?.(key),
      onCancel: () => options.onAnswerDialog?.(null),
    },
    (index) => store.dispatch({ type: "dialog/choose", index }),
    () => store.dispatch({ type: "dialog/toggle" }),
  );

  const statusBar = createStatusBar({
    onAction: (action) => options.onStripAction?.(action),
    onOpenFeed: () => toggleFeed(),
  });

  const main = el(
    "div",
    { class: "fd-main", attrs: { "aria-label": "Terminal" } },
    [tabs.el, pane.el, feed.el],
  );
  const body = el("div", { class: "fd-body" }, [sidebar.el, main]);
  const frame = el(
    "div",
    {
      class: "fd-frame",
      attrs: { "data-mode": "app", "data-layout": "single" },
    },
    [
      logo.el,
      projects.el,
      body,
      gitBar.el,
      statusBar.el,
      takeover.el,
      access.el,
      palette.el,
      config.el,
      dialog.el,
    ],
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
    feed,
    takeover,
    access,
    palette,
    config,
    dialog,
  ];

  function render(state: AppState): void {
    frame.setAttribute("data-mode", state.mode);
    frame.setAttribute("data-layout", state.layout);
    /**
     * The two attributes the overlays hang off. `data-access` also hides the
     * git bar and status bar: with no session there is nothing honest for
     * either to say, and 2b replaces both with its own footer strip.
     */
    frame.setAttribute("data-access", String(state.access !== null));
    frame.setAttribute("data-takeover", String(state.takeover !== null));
    frame.setAttribute("data-feed", String(state.feedOpen));
    /** D13: the dialog layer, like the access and takeover layers. */
    frame.setAttribute("data-dialog", String(state.dialog !== null));

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
   *     the browser's own Ctrl-g never fires on a FlightDeck screen, and it
   *     toggles the command palette (1d, `remote-control-ll5.2`) — the one
   *     thing this chord is allowed to do, per §5.
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
      /**
       * Opening pre-session (no snapshot yet, or mid access/takeover prompt)
       * would show a palette with nothing real to run — the chord still gets
       * swallowed either way, which is the whole point of claiming it.
       */
      if (state.access === null && state.takeover === null) {
        store.dispatch({
          type: state.palette === null ? "palette/open" : "palette/close",
        });
      }
      return;
    }

    /**
     * The overlays claim the keyboard before the main screen does, in the order
     * they are stacked. An access screen is the whole app for as long as it is
     * up (there is nothing to type into behind it), and a takeover prompt is a
     * decision the user has to make before their keys mean anything.
     */
    if (state.access !== null && accessKey(event, state)) {
      return;
    }
    if (state.takeover !== null && takeoverKey(event, state)) {
      return;
    }
    /**
     * A dialog outranks the palette and the configuration manager: it is a
     * question somebody asked that has to be answered before the keyboard means
     * anything, which is the same argument the takeover prompt above makes. It
     * sits *below* access and takeover because those two are about whether this
     * tab may act at all.
     */
    if (state.dialog !== null && dialogKey(event, state)) {
      return;
    }
    if (state.palette !== null && paletteKey(event, state)) {
      return;
    }
    if (state.config !== null && configKey(event, state)) {
      return;
    }
    if (state.feedOpen && feedKey(event)) {
      return;
    }

    /** 2e: `a` opens the feed in App mode. Not in Terminal mode, where `a` is
     * a letter the agent is waiting for. */
    if (isPlain(event) && event.key === "a" && state.mode === "app") {
      event.preventDefault();
      toggleFeed();
      return;
    }

    /**
     * 2c's `r Retry now`, gated on the one state that offers it. `r` is a
     * letter, so it may only be claimed where nothing is listening for letters
     * — and `disconnected` is by definition that: "nothing you type will
     * arrive".
     */
    if (isPlain(event) && event.key === "r" && state.connection === "disconnected") {
      event.preventDefault();
      options.onStripAction?.({
        key: "r",
        label: "Retry now",
        kind: "retry",
        tone: "alert",
      });
      return;
    }

    /** 2c's `Enter Enter a code`, in the one state where `Enter` is free:
     * revoked means no input is being delivered anywhere. */
    if (
      isPlain(event) &&
      event.key === "Enter" &&
      state.connection === "revoked" &&
      state.access === null
    ) {
      event.preventDefault();
      store.dispatch({
        type: "access/required",
        screen: "code_entry",
        attemptsRemaining: null,
        lockoutSeconds: null,
      });
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
   * 2b's keyboard. Digits fill the four boxes, `Backspace` takes one back,
   * `Enter` submits a full code — and on a screen that takes no code, `Enter`
   * starts a new one and `Esc` puts the overlay away without touching the
   * credential.
   */
  function accessKey(event: KeyboardEvent, state: AppState): boolean {
    const current = state.access;
    if (current === null || !isPlain(event)) {
      return false;
    }
    const copy = accessCopy(current);
    /**
     * `Tab` is never ours. Both overlays are operable by pointer *and* by
     * keyboard — the buttons are real buttons — and swallowing `Tab` would
     * leave a keyboard-only user with a panel they can see and cannot reach.
     */
    if (event.key === "Tab") {
      return false;
    }
    if (copy.acceptsCode && /^[0-9]$/.test(event.key)) {
      event.preventDefault();
      store.dispatch({ type: "access/digit", digit: event.key });
      return true;
    }
    if (copy.acceptsCode && event.key === "Backspace") {
      event.preventDefault();
      store.dispatch({ type: "access/backspace" });
      return true;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      if (copy.acceptsCode) {
        submitCode();
      } else if (copy.primary !== null) {
        store.dispatch({ type: "access/retry" });
      }
      return true;
    }
    if (event.key === "Escape" && copy.secondary !== null) {
      event.preventDefault();
      store.dispatch({ type: "access/dismiss" });
      return true;
    }
    /** Swallow everything else: a stray keystroke must not reach the terminal
     * behind an access screen, which is a photograph anyway. */
    return true;
  }

  /** 2f: `Enter` take over · `w` watch read-only · `Esc` cancel (arriving). */
  function takeoverKey(event: KeyboardEvent, state: AppState): boolean {
    if (state.takeover === null || !isPlain(event) || event.key === "Tab") {
      return false;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      store.dispatch({ type: "takeover/claim" });
      return true;
    }
    if (event.key === "w") {
      event.preventDefault();
      store.dispatch({ type: "takeover/observe" });
      return true;
    }
    if (event.key === "Escape" && state.takeover.kind === "arriving") {
      event.preventDefault();
      /** Cancel leaves a live read-only view (2f) — the same destination as
       * `w`, which is why there is no third action to dispatch. */
      store.dispatch({ type: "takeover/observe" });
      return true;
    }
    return true;
  }

  /**
   * 1d: type to filter, `↑↓` move, `Tab` next column, `Enter` run, `Esc`
   * close. Unlike 2b's access screens, `Tab` is claimed here — 1d's own
   * footer names it as a keybinding (`Tab next column`), not an
   * accessibility escape hatch, and the palette's rows are still reachable by
   * pointer for anyone who wants that instead.
   */
  function paletteKey(event: KeyboardEvent, state: AppState): boolean {
    if (state.palette === null) {
      return false;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      store.dispatch({ type: "palette/close" });
      return true;
    }
    if (event.key === "Tab") {
      event.preventDefault();
      store.dispatch({ type: "palette/nextColumn" });
      return true;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      store.dispatch({ type: "palette/move", delta: -1 });
      return true;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      store.dispatch({ type: "palette/move", delta: 1 });
      return true;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      const command = highlightedCommand(store.getState());
      if (command !== null) {
        runCommand(command);
      }
      return true;
    }
    if (event.key === "Backspace") {
      event.preventDefault();
      store.dispatch({ type: "palette/backspace" });
      return true;
    }
    /** A single printable character with no modifier. `event.key.length === 1`
     * is what tells a letter apart from `Shift`/`ArrowLeft`/etc, all of which
     * are otherwise-unhandled multi-character key names that fall through to
     * the swallow below. */
    if (isPlain(event) && event.key.length === 1) {
      event.preventDefault();
      store.dispatch({ type: "palette/type", char: event.key });
      return true;
    }
    /** Swallow everything else: the palette is the whole keyboard while it is
     * open, same posture as 2b's access screens. */
    return true;
  }

  /** `Enter`, or a click on a row (`commandPalette.ts`). Sending the frame and
   * finding out what happened to it are `main.ts`'s job — see
   * `AppOptions.onRunCommand`. */
  function runCommand(command: PaletteCommand): void {
    /**
     * "Open Configuration" (1f, `remote-control-ll5.6`) never leaves the
     * browser — same as `Ctrl-g` opening *this* palette, it is a local UI
     * toggle, not a `Command` frame. The palette closes as it hands off, so
     * only one full-screen overlay is ever up at once.
     *
     * The *row* is still the host's, like every other row
     * (`remote-control-ll5.12`): a host that stops offering the name stops
     * offering the manager. This is only about where the answer comes from —
     * and the host's own refusal for the name says the same thing, that the
     * configuration manager is a browser surface of its own.
     */
    if (command.id === "open_configuration") {
      store.dispatch({ type: "palette/close" });
      store.dispatch({ type: "config/open" });
      return;
    }
    options.onRunCommand?.(command);
  }

  /**
   * 1f: `↑↓` move, `Tab` switch scope, `c` clear a project override, `s`
   * save, `Esc` close. `e` is claimed and swallowed — the footer's `host
   * only` badge is the whole story from a browser (D16); there is nothing
   * for this build to run.
   */
  function configKey(event: KeyboardEvent, state: AppState): boolean {
    if (state.config === null || !isPlain(event)) {
      return false;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      store.dispatch({ type: "config/close" });
      return true;
    }
    if (event.key === "Tab") {
      event.preventDefault();
      store.dispatch({ type: "config/scope" });
      return true;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      store.dispatch({ type: "config/move", delta: -1 });
      return true;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      store.dispatch({ type: "config/move", delta: 1 });
      return true;
    }
    if (event.key === "c") {
      event.preventDefault();
      store.dispatch({ type: "config/clear" });
      return true;
    }
    if (event.key === "s") {
      event.preventDefault();
      saveConfig();
      return true;
    }
    /** Swallow everything else — same posture as the palette and the access
     * screens: this overlay is the whole keyboard while it is open. */
    return true;
  }

  /**
   * 1e's keyboard: `↑↓` move the agent radio, printable characters type the
   * branch, `Backspace` takes one back, `Tab` toggles run-from-base, `Enter`
   * confirms, `Esc` cancels — and a keyed button (`y`, `1`, `i`) fires that
   * button directly, exactly as it does on the desktop.
   *
   * `Esc` here does **not** close the overlay locally. It sends
   * `dialog_cancel`; the host closes the dialog on both surfaces and the panel
   * goes away when `Delta::DialogClosed` arrives. That is the one place this
   * overlay's keyboard differs in kind from the other four, and it is D13's
   * "no new state" made literal.
   */
  function dialogKey(event: KeyboardEvent, state: AppState): boolean {
    const dialog = state.dialog;
    if (dialog === null || !isPlain(event)) {
      return false;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      options.onAnswerDialog?.(null);
      return true;
    }
    if (event.key === "Tab") {
      /** Only claimed by a dialog that has the option; otherwise `Tab` stays
       * the browser's, so a keyboard-only user can still reach the buttons. */
      if (!hasToggle(dialog)) {
        return false;
      }
      event.preventDefault();
      store.dispatch({ type: "dialog/toggle" });
      return true;
    }
    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      event.preventDefault();
      store.dispatch({
        type: "dialog/move",
        delta: event.key === "ArrowUp" ? -1 : 1,
      });
      return true;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      const primary = primaryKey(dialog);
      options.onAnswerDialog?.(primary === null ? "Enter" : primary.key);
      return true;
    }
    if (event.key === "Backspace") {
      event.preventDefault();
      store.dispatch({ type: "dialog/backspace" });
      return true;
    }
    if (event.key.length === 1) {
      event.preventDefault();
      /**
       * One character, two possible meanings, and the dialog decides which:
       * a dialog with a text field is being typed into, and a dialog without
       * one is offering keyed buttons (`y`/`n`, `1`..`9`, `i`/`w`/`b`/`d`) —
       * the same split the desktop's own prompt handlers make.
       */
      if (dialog.input !== null) {
        store.dispatch({ type: "dialog/type", char: event.key });
        return true;
      }
      const pressed = decidingKeys(dialog).find(
        (button) => button.key === event.key,
      );
      if (pressed !== undefined) {
        options.onAnswerDialog?.(pressed.key);
      }
      return true;
    }
    /** Swallow everything else: the dialog is the whole keyboard while it is
     * open, the same posture as the palette and the access screens. */
    return true;
  }

  /**
   * Turns whatever is staged in `state.config.edits` into one `save_config`
   * command. A no-op with nothing staged: `s` on a config that was never
   * touched has nothing honest to send, and sending an empty change set would
   * still cost a round trip to say so.
   */
  function saveConfig(): void {
    const config = store.getState().config;
    if (config === null) {
      return;
    }
    const changes = Object.entries(config.edits).map(([key, edit]) => ({
      key,
      action: edit.kind,
    }));
    if (changes.length === 0) {
      return;
    }
    options.onSaveConfig?.({ scope: config.scope, changes });
  }

  /** 2e's footer: `a close`. `Esc` closes it too — it is a slide-over, and
   * every slide-over in every app closes on `Esc`. */
  function feedKey(event: KeyboardEvent): boolean {
    if (!isPlain(event)) {
      return false;
    }
    if (event.key === "a" || event.key === "Escape") {
      event.preventDefault();
      store.dispatch({ type: "feed/set", open: false });
      return true;
    }
    return false;
  }

  /**
   * Opening the feed marks what it shows as read, which is what makes the
   * unread chip drain when you look at it. The host owns the authoritative
   * record (`Delta::Activity` carries `read`), so this is the local half and
   * `remote-control-hgqy` sends the matching frame from `onDispatch`.
   */
  function toggleFeed(): void {
    const state = store.getState();
    const open = !state.feedOpen;
    store.dispatch({ type: "feed/set", open });
    if (open) {
      const unread = state.activity
        .filter((event) => !event.read)
        .map((event) => event.id);
      if (unread.length > 0) {
        store.dispatch({ type: "activity/read", ids: unread });
      }
    }
  }

  function submitCode(): void {
    const state = store.getState();
    if (state.access === null || !canSubmit(state.access)) {
      return;
    }
    options.onSubmitCode?.(state.access.code);
  }

  /**
   * D3 across projects. A feed row names a session that is usually *not* in the
   * selected project, so this dispatches `selection/jump` — and because
   * selection is instance-wide, it moves the desktop too, which is what the
   * row's own hover copy says it will.
   */
  function jumpTo(event: ActivityEvent): void {
    store.dispatch({
      type: "selection/jump",
      projectId: event.projectId,
      sessionId: event.sessionId,
    });
    store.dispatch({ type: "feed/set", open: false });
  }

  /**
   * A key with no modifier. Every single-letter rule above has to check this,
   * or `Ctrl-a`/`Cmd-r` would be swallowed and the user would lose select-all
   * and reload — two things a browser is entitled to keep.
   */
  function isPlain(event: KeyboardEvent): boolean {
    return !event.ctrlKey && !event.metaKey && !event.altKey;
  }

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
    /**
     * An overlay is not "outside the terminal" in the sense this rule means —
     * it is *over* it. Clicking the feed, an access keypad or a takeover button
     * must not also release the keys, or every choice made in an overlay would
     * quietly change the mode underneath it.
     */
    /**
     * `composedPath()`, not `contains()`. The path is fixed when the event is
     * dispatched, and an overlay's own handler has usually re-rendered its
     * list by the time this bubble arrives — a feed row that dispatched
     * `selection/jump` is *detached* from the DOM before we get here, so
     * `contains()` would answer "not in the feed" and quietly release the keys.
     */
    const path = event.composedPath();
    if (
      path.includes(feed.el) ||
      path.includes(access.el) ||
      path.includes(takeover.el) ||
      path.includes(palette.el) ||
      path.includes(config.el) ||
      path.includes(dialog.el)
    ) {
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
