import { accessCopy, canSubmit } from "../state/access";
import {
  atNameStep,
  cancelButton,
  decidingKeys,
  gateSatisfied,
  gatedKey,
  hasToggle,
  primaryKey,
} from "../state/dialog";
import { highlightedCommand } from "../state/commands";
import type { PaletteCommand } from "../state/commands";
import { stagedChanges } from "../state/config";
import type { ConfigSaveRequest } from "../state/config";
import { createInitialState } from "../state/types";
import type { AppAction, AppState } from "../state/types";
import type { StripAction } from "../state/connection";
import { findProject, findSession } from "../state/model";
import type { ActivityEvent } from "../state/model";
import { createAccessScreen } from "./accessScreen";
import { createActivityFeed } from "./activityFeed";
import { createCommandPalette } from "./commandPalette";
import { createConfigManager } from "./configManager";
import { createDialog } from "./dialog";
import { createGitBar } from "./gitBar";
import { createInfoOverlay } from "./infoOverlay";
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
 * | `takeover/dismiss` | nothing — `Esc` closes the panel and keeps the seat |
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
   * `SessionSocket.sendCommand(OPEN_CONFIGURATION_COMMAND, request)`, gets
   * back the seq the transport assigned, and dispatches `config/dispatched`
   * with it. The host's answer is a fresh `configuration` frame, which is what
   * repaints the rows (§6.5 R22).
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
    /** D14 as revised: cancelling is not a dead end, and it is no longer the
     * same act as `w`. Being refused a keystroke costs the turn and not the
     * seat, so `Esc` means *I will wait* — the panel closes and this tab is
     * still a writer. See `takeover/dismiss`. */
    onCancel: () => store.dispatch({ type: "takeover/dismiss" }),
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
   * The three read-only panels (`remote-control-ll5.8`): help, About, and
   * SPECS §21's git status. Opened by their palette rows below — and help also
   * by `?` in App mode — closed by `Esc` or by clicking outside.
   *
   * Two of the three never leave the browser, exactly as "Open Configuration"
   * does not: their content is already on the snapshot. The third *does* send
   * a frame, because its facts are a fresh `git` read the snapshot cannot
   * hold, and the panel opens when the host's answer arrives rather than when
   * the request goes out — see `wire/socket.ts`'s `onGitStatus`.
   */
  const info = createInfoOverlay({
    onClose: () => store.dispatch({ type: "readOnly/close" }),
  });
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
    (key) => options.onAnswerDialog?.(key),
    /** 1g step 1 → step 2. Local, and sends nothing: pressing the destructive
     * button from a browser opens the name field, it does not answer. */
    () => store.dispatch({ type: "dialog/advance" }),
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
  /**
   * 1h: *"below 900px … the git bar folds into the status bar."*
   *
   * The fold is this wrapper and nothing else. At wide it is
   * `display: contents`, so the two strips are laid out by `.fd-frame`
   * exactly as they were before it existed — the wide layout is unchanged to
   * the pixel. At narrow it becomes a `column-reverse` flex box, which puts
   * the status line on top of the git line inside one box with one border:
   * two strips become one bar, which is what "folds into" describes.
   *
   * `column-reverse` rather than reordering the DOM, for three reasons worth
   * keeping: the status bar's `border-top` is 2c's frame colour and stays the
   * *top* edge of the combined bar, so rule 3 ("the whole bar takes the
   * state's colour") survives untouched; the DOM order is still git-then-
   * status, so a screen reader hears the same order at both widths; and the
   * git bar has no focusable content, so nothing's tab order is inverted by
   * the visual flip.
   */
  const footer = el("div", { class: "fd-footer" }, [gitBar.el, statusBar.el]);
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
      footer,
      takeover.el,
      access.el,
      palette.el,
      config.el,
      info.el,
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
    info,
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
    /** `remote-control-ll5.8`: whichever read-only panel is up, or `none`. */
    frame.setAttribute("data-readonly", state.readOnly?.kind ?? "none");
    /**
     * 1h's breakpoint, and 1h's slide-over (`remote-control-eek.4`, §6.5 R17).
     *
     * The eighth and ninth attributes on this element, and they are here for
     * the reason the other seven are: a layout the app can *reason about* is a
     * layout a test can assert. The 900px decision itself is `widthClass`, a
     * pure function of the pixel width `main.ts` measures — `src/style/
     * narrow.css` reads only the answer, so there is no width media query
     * anywhere in the app and jsdom can drive the whole narrow layout.
     */
    frame.setAttribute("data-width", state.width);
    frame.setAttribute("data-sidebar", String(state.sidebarOpen));
    /**
     * 1h position 4 (`remote-control-ecsv`, §6.5 R24): the tenth attribute, and
     * the last piece of `[ui] agent_tab_position`. `main.css` reads it and
     * mirrors the body row; nothing here measures or branches, exactly as
     * `data-width` above hands `narrow.css` an answer rather than a media
     * query.
     */
    frame.setAttribute("data-sidebar-side", state.sidebarPosition);

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
   *
   * ## Why this listens on the document, not on the frame
   *
   * `remote-control-eek.4` (§6.5 R17). It was on `frame`, and a keydown only
   * reaches a listener on an element if that element is an **ancestor of the
   * focused one**. `document.body` is an ancestor of `.fd-frame`, not a
   * descendant — so with focus on the body, every key below was delivered
   * nowhere.
   *
   * That is not an edge case, it is the default: `activeElement` is `BODY` on a
   * fresh load, and it returns to `BODY` whenever a focused control is removed
   * from the DOM — which every control here is, because each region rebuilds
   * its children on every render. Measured in a real browser: on a freshly
   * loaded tab **no app-level key worked at all** — not `Ctrl-g`, the one chord
   * §5 gives the app, not `Esc Esc`, not 2e's `a`, not R16's `?` — until the
   * user happened to click the terminal. Clicking any chrome control then
   * silently took them away again.
   *
   * Keys have no position. Their target is wherever focus happens to be, which
   * is not this component's business, so the keyboard belongs to the document.
   * `Ctrl-g` alone uses capture on the frame because xterm consumes terminal
   * keys before a bubbling document listener can see them. The chord FlightDeck
   * claims must be handled on the way down, before xterm can turn it into a BEL
   * byte for the hosted agent. The document listener remains the fallback when
   * focus is on `body`, outside the frame's event path.
   * The **pointer** handler below stays on the frame, because a click does have
   * a position and its target is a real element inside it.
   *
   * `isConnected` is the whole of the tidying that costs: a frame taken out of
   * the page must not answer for keys any more. In production nothing removes
   * it; in `vitest` a file renders a dozen apps into one jsdom document, and
   * without this each one's listener would outlive its DOM and keep reducing
   * into a store nobody is reading.
   */
  frame.addEventListener("keydown", handleCommandPaletteShortcut, true);

  document.addEventListener("keydown", (event: KeyboardEvent) => {
    if (!frame.isConnected) {
      return;
    }
    const state = store.getState();

    if (handleCommandPaletteShortcut(event)) {
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
    /**
     * The read-only panels sit below the palette and the configuration
     * manager in this order for the same reason those two sit below a dialog:
     * ranking is by *how much is at stake*, and a panel that states facts and
     * asks nothing is the least urgent thing on screen. In practice the order
     * never has to arbitrate — the reducer closes the palette when a panel
     * opens, so only one of them is ever up.
     */
    if (state.readOnly !== null && readOnlyKey(event)) {
      return;
    }
    if (state.feedOpen && feedKey(event)) {
      return;
    }
    if (state.sidebarOpen && sidebarKey(event)) {
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
     * 1h's slide-over, on `s` in App mode and only below 900px
     * (`remote-control-eek.4`, §6.5 R17).
     *
     * The gating is 2e's, twice over. *App mode only*, because in Terminal
     * mode `s` is a letter the agent is waiting for — 2e's own words for `a`.
     * *Narrow only*, because at wide the sidebar is 1a's column and is already
     * on screen: a key that toggled nothing would be a key that lies.
     *
     * §5 gives the app one **chord** (`Ctrl-g`) and this is not one; a plain
     * key in App mode is the affordance 2e licenses and R16 already took a
     * second helping of for `?`. `s` is free there, and it is the first letter
     * of the thing it opens.
     *
     * The chip in the project row is the pointer door and needs no key at all,
     * which is the whole of §5's palette-primary position — see
     * `ui/projectTabs.ts`.
     */
    if (
      isPlain(event) &&
      event.key === "s" &&
      state.mode === "app" &&
      state.width === "narrow"
    ) {
      event.preventDefault();
      store.dispatch({ type: "sidebar/set", open: !state.sidebarOpen });
      return;
    }

    /**
     * `?` opens help, in App mode only — `remote-control-ll5.8`, derived from
     * 2e's rule for `a` rather than invented beside it.
     *
     * §5 gives the app **one chord** (`Ctrl-g`) and nothing licenses a second,
     * so the desktop's `F1` / `Alt-h` are deliberately not taken: `F1` is the
     * browser's own help in Chrome and Firefox, and `Alt-h` opens a browser
     * menu on Windows. A plain key in App mode is the affordance the artboards
     * *do* license — 2e claims `a` exactly this way, on exactly this reasoning
     * ("not in Terminal mode, where `a` is a letter the agent is waiting for")
     * — and `?` is free there.
     *
     * The palette's "Show Help" row is the other door and needs no key at all,
     * which is the whole of §5's palette-primary position.
     */
    if (isPlain(event) && event.key === "?" && state.mode === "app") {
      event.preventDefault();
      store.dispatch({ type: "help/open" });
      return;
    }

    /**
     * 1b's `↑↓ move`, 2e's `↑↓ sessions`, and 1h's *"bare-key App-mode nav
     * (`↑↓ ←→ ? Enter`) survives untouched"* — three artboards printing one
     * route that nothing was bound to (`remote-control-qlza`, §6.5 R23). The
     * sidebar was pointer-only, so the footer under it named a key that did
     * nothing.
     *
     * **App mode only**, which is not a hedge but §5's own line: in Terminal
     * mode the arrows are the agent's — history, menus, `less` — and the app
     * has no business reading them there. The gate is the same one `a`, `s`
     * and `?` already sit behind.
     *
     * It dispatches `selection/session`, the *click's* action, so the desktop
     * moves with it (D3) rather than the browser growing a second, local
     * notion of which session is selected.
     */
    if (
      isPlain(event) &&
      (event.key === "ArrowUp" || event.key === "ArrowDown") &&
      state.mode === "app"
    ) {
      event.preventDefault();
      moveSession(state, event.key === "ArrowUp" ? -1 : 1);
      return;
    }

    /**
     * 1c's `←/→ move focus`, bound at last — same reasoning as the row above,
     * and the same App-mode gate for the same §5 reason. 1c draws split view
     * with `MODE: TERMINAL`; the resolution is in `statusBar.ts`'s `hintsFor`,
     * which stops printing the keys where they are not ours rather than taking
     * arrows the agent is waiting for.
     */
    if (
      isPlain(event) &&
      (event.key === "ArrowLeft" || event.key === "ArrowRight") &&
      state.mode === "app" &&
      state.layout === "split"
    ) {
      event.preventDefault();
      moveSplitFocus(state, event.key === "ArrowLeft" ? -1 : 1);
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
        /** Nothing was refused, so there is nothing the host has told us. The
         * screen renders its sentences without the clauses these numbers fill;
         * the first refusal brings them, and they are never remembered from a
         * constant of our own. */
        attemptsRemaining: null,
        lockoutSeconds: null,
        lockoutLengthSeconds: null,
        codeTtlSeconds: null,
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

  function handleCommandPaletteShortcut(event: KeyboardEvent): boolean {
    if (!frame.isConnected || !isCommandPaletteShortcut(event)) {
      return false;
    }
    event.preventDefault();
    event.stopPropagation();
    const state = store.getState();
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
    return true;
  }

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
      /** Cancel leaves a live view (2f) — but under D14 as revised a live
       * *writing* one: the refusal cost the turn, not the seat, so `Esc` means
       * "I will wait" and is a different destination from `w`. */
      store.dispatch({ type: "takeover/dismiss" });
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
     * Help and About are intercepted (`remote-control-ll5.8`): their content is
     * already on the snapshot (`Snapshot::help` / `Snapshot::about`), so the
     * answer is in hand and a round trip would buy nothing — while
     * *forwarding* the row would open a panel on the desktop that the person
     * who asked cannot read. The host's own refusals (`HELP_REFUSAL`,
     * `ABOUT_REFUSAL`) say the same thing, which is what keeps this local
     * interception honest rather than a shortcut: a host that stops offering
     * the name stops offering the panel, because the *row* is still the host's
     * like every other (R7).
     *
     * Two rows are deliberately **not** here, and they are the two whose facts
     * the browser does not have. `show_git_status` goes to the host like any
     * other command and the panel opens when the answer arrives; so does
     * `open_configuration` as of `remote-control-1p22`, because SPECS §8's
     * layering is a read of two files on the host's disk and a browser that
     * drew it locally would be drawing nobody's machine (§6.5 R22).
     */
    if (command.id === "show_help") {
      store.dispatch({ type: "help/open" });
      return;
    }
    if (command.id === "about_flightdeck") {
      store.dispatch({ type: "about/open" });
      return;
    }
    options.onRunCommand?.(command);
  }

  /**
   * The read-only panels' whole keyboard: `Esc`, and `?` to close the help
   * panel the same key opened (2e's `a` toggles the feed exactly this way).
   *
   * Everything else is **swallowed**, the same posture the palette, the
   * configuration manager and the access screens take. The panel sits over a
   * scrim that already covers the frame to the pointer, so letting `a` open
   * the activity feed *behind* it — or `Enter` move the mode underneath it —
   * would leave the reader looking at a panel while the app quietly did
   * something else.
   *
   * Nothing is lost by swallowing. §5.1's "queued, never dropped" is about
   * keystrokes bound for the PTY, and those do not come through this handler
   * at all: since `remote-control-hgqy` terminal input comes from xterm's own
   * `onData` (R5), so a key eaten here was only ever going to be one of this
   * frame's own app-level shortcuts.
   *
   * `Tab` is the one exception, as it is on 2b's access screens: both panels
   * are operable by pointer *and* by keyboard, and swallowing `Tab` would
   * leave a keyboard-only reader with a link and a close button they can see
   * and cannot reach.
   */
  function readOnlyKey(event: KeyboardEvent): boolean {
    if (!isPlain(event) || event.key === "Tab") {
      return false;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      store.dispatch({ type: "readOnly/close" });
      return true;
    }
    if (event.key === "?" && store.getState().readOnly?.kind === "help") {
      event.preventDefault();
      store.dispatch({ type: "readOnly/close" });
      return true;
    }
    /** Swallow the rest — see the doc comment. */
    return true;
  }

  /**
   * 1f: `↑↓` move, `Space` toggle or open the inline editor, `Tab` switch
   * scope, `c` clear the override, `s` save, `Esc` close. `e` is claimed and
   * swallowed — the footer's `host only` badge is the whole story from a
   * browser (D16); `$EDITOR` opens on the host's screen.
   *
   * `Space` is bound as of `remote-control-1p22`. 1f's footer has promised
   * `Space toggle / edit` since turn 1 and nothing was listening; it does the
   * same two things the desktop's `Space` does, because it stages the value the
   * host's own field definition says comes next and opens an editor seeded with
   * the value in effect.
   */
  function configKey(event: KeyboardEvent, state: AppState): boolean {
    const config = state.config;
    if (config === null || !isPlain(event)) {
      return false;
    }
    /** An open inline edit takes the keyboard whole, exactly as it does in
     * `handle_config_key`: type to insert, `Backspace` to delete, `Enter` to
     * commit, `Esc` to discard — and nothing else fires until it is resolved,
     * so `s` cannot save half a relay URL and `Tab` cannot carry it into the
     * other scope. */
    if (config.editing !== null) {
      event.preventDefault();
      if (event.key === "Escape") {
        store.dispatch({ type: "config/editCancel" });
      } else if (event.key === "Enter") {
        store.dispatch({ type: "config/editCommit" });
      } else if (event.key === "Backspace") {
        store.dispatch({ type: "config/editBackspace" });
      } else if (event.key.length === 1) {
        store.dispatch({ type: "config/editType", char: event.key });
      }
      return true;
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
    /** One key, two acts, exactly as `toggle_selected` has them: a toggle or a
     * choice takes its next value; a text field opens the editor. Which of the
     * two happens is decided by the *host's* `kind` for the row — the reducer
     * asks it, and neither branch is a list this browser keeps. */
    if (event.key === " " || event.key === "Enter") {
      event.preventDefault();
      store.dispatch({ type: "config/activate" });
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
    /** Artboard 1g's step 2 takes the keyboard whole: the panel is one field
     * and two keys. `Esc` above it is deliberately *first*, so cancelling out
     * of a half-typed name never needs the name (R8). */
    if (atNameStep(dialog)) {
      event.preventDefault();
      if (event.key === "Enter") {
        /** A confirm the host would refuse is not sent: the browser does not
         * spend a round trip to be told what it can already see. The host
         * checks the same name anyway — this is the affordance, not the
         * enforcement. */
        if (gateSatisfied(dialog) && dialog.gate !== null) {
          options.onAnswerDialog?.(dialog.gate.key);
        }
        return true;
      }
      if (event.key === "Backspace") {
        store.dispatch({ type: "dialog/gateBackspace" });
        return true;
      }
      if (event.key.length === 1) {
        store.dispatch({ type: "dialog/gateType", char: event.key });
      }
      return true;
    }
    if (event.key === "Tab") {
      /** Only claimed by a dialog that has the option; otherwise `Tab` stays
       * the browser's, so a keyboard-only user can still reach the buttons. */
      if (!hasToggle(dialog)) {
        return false;
      }
      event.preventDefault();
      const toggle = dialog.buttons.find((button) => button.key === "Tab");
      if (toggle !== undefined) {
        options.onAnswerDialog?.(toggle.key);
      }
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
      const key = primary === null ? "Enter" : primary.key;
      if (gatedKey(dialog, key)) {
        store.dispatch({ type: "dialog/advance" });
        return true;
      }
      options.onAnswerDialog?.(key);
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
      /** The host's own cancel key (`n` in the close confirmations, `c` in the
       * push confirmation) still cancels, and it cancels through
       * `dialog_cancel` — the frame that is never gated and never refused —
       * rather than through a confirm carrying that key. It is no longer a
       * *deciding* key, because the panel no longer draws it as a second
       * button beside `Esc Cancel` (§6.5 R19). */
      if (cancelButton(dialog)?.key === event.key) {
        options.onAnswerDialog?.(null);
        return true;
      }
      const pressed = decidingKeys(dialog).find(
        (button) => button.key === event.key,
      );
      if (pressed !== undefined) {
        /** The gated key (1g's `y`) opens step 2 instead of answering — the
         * same thing clicking the button does, because they are the same
         * press. */
        if (gatedKey(dialog, pressed.key)) {
          store.dispatch({ type: "dialog/advance" });
        } else {
          options.onAnswerDialog?.(pressed.key);
        }
      }
      return true;
    }
    /** Swallow everything else: the dialog is the whole keyboard while it is
     * open, the same posture as the palette and the access screens. */
    return true;
  }

  /**
   * Turns whatever is staged in `state.config.edits` into one
   * `open_configuration` frame. A no-op with nothing staged: `s` on a config
   * that was never touched has nothing honest to send, and an empty change set
   * would cost a round trip to be told so.
   *
   * Both scopes travel in one frame, because the desktop's `s` writes both
   * dirty files — each change carries the scope it belongs to
   * (`stagedChanges`), so a browser is not the lesser manager wearing the same
   * footer.
   */
  function saveConfig(): void {
    const config = store.getState().config;
    if (config === null) {
      return;
    }
    const changes = stagedChanges(config.doc, config.edits);
    if (changes.length === 0) {
      return;
    }
    options.onSaveConfig?.({ changes });
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
   * 1h's slide-over closes on `s` — the key that opened it, exactly as 2e's
   * `a` toggles the feed — and on `Esc`, because every slide-over in every app
   * closes on `Esc`.
   *
   * It **swallows nothing else**, which is the difference between this and the
   * palette's keyboard. The sidebar is 1a's column temporarily overlaid, not a
   * question: `↑↓` still move the session selection and `Enter` still focuses
   * the terminal while it is up, which is the same posture `feedKey` takes
   * directly above and for 2e's reason — no scrim, no focus trap, nothing
   * behind it stopped being live.
   */
  function sidebarKey(event: KeyboardEvent): boolean {
    if (!isPlain(event)) {
      return false;
    }
    if (event.key === "s" || event.key === "Escape") {
      event.preventDefault();
      store.dispatch({ type: "sidebar/set", open: false });
      return true;
    }
    return false;
  }

  /**
   * The sidebar's `↑↓`, in the one shape the sidebar's own click already uses.
   *
   * **It clamps, it does not wrap.** Selection is instance-wide (D3), so a
   * wrap would carry the desktop from the last agent back to the first on a
   * keystroke meant to do nothing — a surprise that costs more than the
   * keypress saves. Movement is within the selected project only, which is
   * what `selection/session` accepts; crossing projects is `selection/jump`'s
   * job and belongs to the feed row that names another project by name.
   */
  function moveSession(state: AppState, delta: number): void {
    const selection = state.selection;
    if (selection === null) {
      return;
    }
    const sessions =
      findProject(state.projects, selection.projectId)?.sessions ?? [];
    const index = sessions.findIndex((s) => s.id === selection.sessionId);
    const next = index === -1 ? undefined : sessions[index + delta];
    if (next === undefined) {
      return;
    }
    store.dispatch({ type: "selection/session", sessionId: next.id });
  }

  /**
   * 1c's `←/→`, dispatching exactly the pair a click on a column dispatches
   * (`ui/splitView.ts`): the focused column *and* the selected terminal, which
   * is one fact told to two places. Clamped at both ends for `moveSession`'s
   * reason — and because a column index outside the session's terminals would
   * focus a column that is not on screen.
   */
  function moveSplitFocus(state: AppState, delta: number): void {
    const selection = state.selection;
    const session =
      selection === null
        ? null
        : findSession(state.projects, selection.projectId, selection.sessionId);
    const columns = session?.terminals.length ?? 0;
    if (session === null || columns === 0) {
      return;
    }
    /**
     * Clamped into range *before* stepping. Moving to a session with fewer
     * terminals leaves `splitFocus` past the last column it drew, and an arrow
     * that silently did nothing there would be the very defect this binding
     * was written to remove.
     */
    const from = Math.min(state.splitFocus, columns - 1);
    const column = Math.min(Math.max(from + delta, 0), columns - 1);
    const terminal = session.terminals[column];
    if (terminal === undefined || column === state.splitFocus) {
      return;
    }
    store.dispatch({ type: "split/focus", column });
    store.dispatch({ type: "selection/terminal", terminalId: terminal.id });
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

  /** `code` covers Chromium-family shells that normalize a modified key
   * differently while still reporting the physical G key. Modifier checks keep
   * browser-owned variants such as `Ctrl-Shift-g` out of FlightDeck's claim. */
  function isCommandPaletteShortcut(event: KeyboardEvent): boolean {
    return (
      event.ctrlKey &&
      !event.metaKey &&
      !event.altKey &&
      !event.shiftKey &&
      (event.key.toLowerCase() === "g" || event.code === "KeyG")
    );
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
    /**
     * A read-only panel is the one overlay where clicking *outside* it means
     * something more than "not in the panel": it means the reader is done with
     * it. Closing it here is the pointer half of `Esc`, and it happens before
     * the mode rule below so the click that dismissed the panel does not also
     * release the keys.
     */
    if (store.getState().readOnly !== null) {
      if (path.includes(info.el)) {
        return;
      }
      store.dispatch({ type: "readOnly/close" });
      return;
    }
    /**
     * 1h's slide-over: the pointer half of `Esc`, and the one gesture a phone
     * has for "put that away". Unlike the read-only panels above it does
     * **not** `return` — the click keeps travelling to the mode rule below, so
     * tapping the terminal to dismiss the sidebar also focuses the terminal.
     * On a touch screen, needing two taps for one intention is the bug.
     *
     * The project row is exempt, and for two reasons rather than one. It holds
     * the chip that *opens* the panel (1h), so a click there is the sidebar's
     * own control and not a click outside it — without this the chip would
     * open the panel and then this handler, running as the same click bubbled
     * up, would close it again. And a click on a project *tab* with the list
     * open is somebody switching project in order to see that project's
     * sessions, so the list stays up and repopulates.
     */
    if (
      store.getState().sidebarOpen &&
      !path.includes(sidebar.el) &&
      !path.includes(projects.el)
    ) {
      store.dispatch({ type: "sidebar/set", open: false });
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
