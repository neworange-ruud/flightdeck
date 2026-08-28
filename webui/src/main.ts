import { consumeBootstrapCode, windowUrlBar } from "./access/bootstrap";
import { checkSession, exchangeCode } from "./access/client";
import type { AccessResult } from "./access/client";
import { SAVE_CONFIG_COMMAND } from "./state/config";
import {
  cancelArgs,
  confirmArgs,
  DIALOG_CANCEL_COMMAND,
  DIALOG_CONFIRM_COMMAND,
} from "./state/dialog";
import { createApp } from "./ui/app";
import { mountTerminal } from "./term/terminal";
import type { Store } from "./ui/store";
import type { TerminalMount } from "./ui/terminalStage";
import { openSession, type SessionSocket } from "./wire/socket";

/**
 * Entry point.
 *
 * Both halves are real:
 *
 *   - **Access.** The bootstrap code is consumed from the URL fragment and
 *     stripped from history, exchanged at `POST /auth/exchange`, and
 *     `GET /auth/session` decides between the app and one of artboard 2b's
 *     screens.
 *   - **The session.** Once access is granted, `./wire/socket` opens `GET /ws`
 *     and the host drives everything: the snapshot paints the tree, `term_bytes`
 *     goes straight into xterm.js, and a keystroke goes back as an `input`
 *     frame. Nothing on screen is a fixture.
 */

const root = document.querySelector<HTMLDivElement>("#app");
if (root === null) {
  throw new Error("#app mount point missing from index.html");
}

/** `Some` from the moment access is granted; one socket per tab. */
let session: SessionSocket | null = null;

/**
 * D4: construct xterm with the host's grid and letterbox it. `FitAddon` is
 * absent by design — see `src/term/terminal.ts`.
 *
 * The mount is also where the two directions of the terminal meet: the socket's
 * sink writes host bytes in, and xterm's `onData` — which owns the whole
 * keyboard-to-bytes translation, including the escape sequences a hand-written
 * key handler always gets wrong — sends them back out.
 */
const mount: TerminalMount = (container, geometry, terminalId) => {
  const term = mountTerminal(container, geometry);
  /**
   * `Esc` is the one key xterm must **not** claim.
   *
   * `Esc Esc` within 400 ms leaves terminal focus and a single `Esc` passes
   * through to the agent (§5, `decideEscape`), and only the app knows which of
   * the two a given press is. If xterm handled `Escape` itself it would send
   * `\x1b` immediately — before the app could decide — and the second press of
   * an `Esc Esc` would have already gone to the agent. So the app's frame-level
   * handler owns `Escape`, queues it through the store when it is a
   * pass-through, and `flushQueuedInput` below puts it on the wire. Every other
   * key stays xterm's, because xterm owns the keyboard-to-bytes translation.
   */
  term.attachCustomKeyEventHandler(
    (event) => !(event.type === "keydown" && event.key === "Escape"),
  );
  const socket = session;
  if (socket !== null) {
    socket.attachTerminal(terminalId, (bytes) => term.write(bytes));
    term.onData((data) => socket.sendInput(terminalId, data));
  }
  return () => {
    socket?.detachTerminal(terminalId);
    term.dispose();
  };
};

const app = createApp({
  mount,
  /**
   * D3: a selection made here is the whole instance's selection, the desktop
   * included — so it goes out as a `Command` and comes back as the host's own
   * selection, rather than being applied locally and hoping the host agrees.
   */
  onDispatch: (action) => {
    if (session === null) {
      return;
    }
    switch (action.type) {
      case "selection/project":
        session.sendCommand("select_project", { project_id: action.projectId });
        return;
      case "selection/session":
        session.sendCommand("select_session", { session_id: action.sessionId });
        return;
      case "selection/terminal":
        session.sendCommand("select_terminal", {
          terminal_id: action.terminalId,
        });
        return;
      case "selection/jump":
        session.sendCommand("select_session", { session_id: action.sessionId });
        return;
      case "activity/read":
        session.sendCommand("mark_activity_read", { event_ids: action.ids });
        return;
      /** Takeover has no frame of its own: the browser re-sends `Attach`. The
       * socket does that itself when the host refuses the seat. */
      default:
        return;
    }
  },
  onSubmitCode: (code) => {
    void exchange(app.store, code);
  },
  /**
   * 1d, `remote-control-ll5.2`. `sendCommand` returns the seq it minted (it
   * shares the counter with `Input` frames, §5.1), so this is the one place
   * that can correlate the row that was run with the `Ack`/`Error` that
   * settles it — `command/result` is dispatched from `socket.ts` once that
   * arrives, never guessed at here.
   */
  onRunCommand: (command) => {
    if (session === null) {
      return;
    }
    const seq = session.sendCommand(command.run.name, command.run.args);
    app.store.dispatch({
      type: "palette/dispatched",
      seq,
      label: command.label,
    });
  },
  /**
   * 1f, `remote-control-ll5.6`. `SAVE_CONFIG_COMMAND` is a placeholder —
   * protocol v1 has no `save_config` command yet (see `state/config.ts`'s
   * module doc and the ll5.6 task report for the shape `remote-control-ll5.1`
   * needs to add). Sending it today gets whatever the host does with an
   * unrecognised command name, and `command/result` renders exactly that —
   * never a fabricated success.
   */
  onSaveConfig: (request) => {
    if (session === null) {
      return;
    }
    const seq = session.sendCommand(SAVE_CONFIG_COMMAND, request);
    app.store.dispatch({ type: "config/dispatched", seq });
  },
  /**
   * D13's shared dialog (1d/1e, `remote-control-ll5.3`). `key` names the button
   * pressed; `null` is a cancel.
   *
   * Two things this deliberately does not do. It does not close the dialog — the
   * host does, and the browser hears about it as a `Delta::DialogClosed`, which
   * is what makes "either surface can confirm or cancel and the other reflects
   * it" one mechanism instead of two. And it does not decide whether the answer
   * is allowed: the host owns that (`confirmable` / `refusal`), so a refusal
   * arrives in the host's own words through `command/result` like every other
   * outcome.
   */
  onAnswerDialog: (key) => {
    const dialog = app.store.getState().dialog;
    if (session === null || dialog === null) {
      return;
    }
    const seq =
      key === null
        ? session.sendCommand(DIALOG_CANCEL_COMMAND, cancelArgs(dialog))
        : session.sendCommand(DIALOG_CONFIRM_COMMAND, confirmArgs(dialog, key));
    app.store.dispatch({
      type: "dialog/dispatched",
      seq,
      act: key === null ? "cancel" : "confirm",
    });
  },
  onStripAction: (action) => {
    if (action.kind === "reload") {
      /** Turn 2 §4: a version mismatch is a stale tab, not a negotiation, and
       * the SPA ships inside the binary (D9) — so the fix really is a reload. */
      window.location.reload();
      return;
    }
    if (action.kind === "code") {
      app.store.dispatch({
        type: "access/required",
        screen: "code_entry",
        attemptsRemaining: null,
        lockoutSeconds: null,
      });
      return;
    }
    /** `retry` — drop the socket and let the session open a fresh one now
     * instead of waiting out the backoff. */
    session?.close();
    session = null;
    startSession();
  },
});

root.append(app.el);

/** 2b's footer prints the address the user actually reached, never a guess. */
app.store.dispatch({ type: "host/set", host: window.location.host });

/** Installed once, not per session: a reconnect must not double the flush. */
app.store.subscribe(flushQueuedInput);

/**
 * Q4, in the order that matters.
 *
 * The fragment is read and **stripped before anything touches the network**.
 * If the strip waited for a successful exchange, every failure — offline, host
 * restarting, code expired, tab closed mid-request — would leave a live
 * credential sitting in the browser's history and in any bookmark made from
 * it. Failing after the strip costs one re-entry on a screen that exists for
 * exactly that; failing before it costs the credential.
 */
void start();

async function start(): Promise<void> {
  const code = consumeBootstrapCode(windowUrlBar());
  if (code !== null) {
    await exchange(app.store, code);
    return;
  }
  /**
   * No code in the URL, so ask whether the cookie we may already hold is
   * still good — over HTTP, not by opening a WebSocket that would be refused.
   * A refused socket would mean a connection error to explain away, and it
   * would spend an attempt against the per-address limiter for nothing.
   */
  applyResult(app.store, await checkSession(), "session");
}

async function exchange(store: Store, code: string): Promise<void> {
  /**
   * `after: "exchange"` matters. A refusal here is a refusal of *digits the
   * user just typed*, so it dispatches `access/refused`, which keeps them for
   * 2b's rejected screen to show — "it expired, or it was mistyped" is only a
   * claim the user can check if the digits are still on screen. A refusal from
   * `GET /auth/session` refused no digits at all.
   */
  applyResult(
    store,
    await exchangeCode(code, { label: navigator.userAgent }),
    "exchange",
  );
}

function applyResult(
  store: Store,
  result: AccessResult,
  after: "exchange" | "session" = "session",
): void {
  if (result.ok) {
    store.dispatch({ type: "access/granted" });
    startSession();
    return;
  }
  if ("unreachable" in result) {
    /**
     * Nothing refused us — nobody answered. Telling the user "that code did
     * not work" when no one looked at it is exactly the kind of claim Q7
     * forbids, so this is a connection state, not an access screen.
     */
    console.warn("[access] host unreachable:", result.detail);
    store.dispatch({ type: "connection/changed", status: "disconnected" });
    return;
  }
  /**
   * The host chose the screen (`AccessScreen`); the browser renders it.
   *
   * `revoked` gets its own action because it also means the *connection* is
   * revoked — 2c's strip has to say `not allowed` behind the panel, and a
   * plain `access/required` would leave it claiming the link is fine.
   */
  if (result.screen === "revoked") {
    /** An HTTP refusal does not say *when* access was withdrawn, so we do not
     * claim to know: `null` prints the sentence without a time. */
    store.dispatch({ type: "access/revoked", revokedAgo: null });
    return;
  }
  store.dispatch({
    type: after === "exchange" ? "access/refused" : "access/required",
    screen: result.screen,
    attemptsRemaining: result.attemptsRemaining,
    lockoutSeconds: result.lockoutSeconds,
  });
}

/**
 * Open the live session. Idempotent: a second call while a socket is already
 * open is a no-op, so `access/granted` arriving twice cannot produce two
 * sockets competing for the controlling seat.
 */
/**
 * Put anything the app queued in the store on the wire, and clear the queue.
 *
 * Only the `Esc` path puts bytes here (the reducer's `input/esc` pass-through);
 * everything else goes straight from xterm's `onData` to the socket. It still
 * has to be drained, and drained *by the transport*, because
 * `state.pendingInput` is what artboard 2d's `N keystrokes held` counts — a
 * queue nobody empties would leave the pane reporting held keystrokes that were
 * in fact delivered, which is exactly the kind of claim §5.1 rules out.
 *
 * Deferred to a microtask so it never dispatches from inside the store's own
 * notification pass.
 */
let flushScheduled = false;
function flushQueuedInput(): void {
  if (flushScheduled || session === null) {
    return;
  }
  const state = app.store.getState();
  if (state.pendingInput.length === 0) {
    return;
  }
  const terminalId = state.selection?.terminalId ?? null;
  if (terminalId === null) {
    return; /** Nothing selected: keep it queued rather than guess a terminal. */
  }
  flushScheduled = true;
  queueMicrotask(() => {
    flushScheduled = false;
    const pending = app.store.getState().pendingInput;
    if (pending.length === 0 || session === null) {
      return;
    }
    session.sendInput(terminalId, pending.join(""));
    app.store.dispatch({ type: "input/flush" });
  });
}

function startSession(): void {
  if (session !== null) {
    return;
  }
  session = openSession({
    store: app.store,
    /**
     * The viewport the browser can currently show, in cells. Reported so the
     * host knows whether this tab is clipping the grid; it is structurally
     * incapable of resizing a PTY (D4).
     */
    viewport: () => {
      const geometry = app.store.getState().geometry;
      if (geometry === null) {
        return null;
      }
      return { cols: geometry.cols, rows: geometry.rows };
    },
  });
}
