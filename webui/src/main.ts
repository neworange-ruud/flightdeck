import { consumeBootstrapCode, windowUrlBar } from "./access/bootstrap";
import { checkSession, exchangeCode } from "./access/client";
import type { AccessResult } from "./access/client";
import { OPEN_CONFIGURATION_COMMAND } from "./state/config";
import {
  cancelArgs,
  confirmArgs,
  DIALOG_CANCEL_COMMAND,
  DIALOG_CONFIRM_COMMAND,
} from "./state/dialog";
import {
  browseRequest,
  cloneRequest,
  openRequest,
  scanRequest,
} from "./state/picker";
import type { PickerIntent, PickerRequest } from "./state/picker";
import {
  beginUploadCommand,
  nextUploadJob,
  uploadRequest,
  uploadResolve,
} from "./state/upload";
import type { Resolution, UploadRequest } from "./state/upload";
import { putUpload, type UploadTransfer } from "./wire/upload";
import {
  beginDownloadCommand,
  downloadBrowseCommand,
  downloadBrowseTarget,
  downloadRequest,
  nextDownloadHandoff,
} from "./state/download";
import type { DownloadIntent } from "./state/download";
import { startDownloads } from "./wire/download";
import {
  cellsFor,
  onScreenTerminalId,
  WORKER_COMMAND,
} from "./state/worker";
import type { NotificationPermissionState } from "./state/worker";
import { createApp } from "./ui/app";
import { mountTerminal } from "./term/terminal";
import type { Terminal } from "@xterm/xterm";
import type { Store } from "./ui/store";
import type { TerminalMount } from "./ui/terminalStage";
import type { WireNotificationView } from "./wire/frames";
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
/** The single pane's mounted terminal, for W5's cell size. */
let paneTerminal: Terminal | null = null;

const mount: TerminalMount = (container, geometry, terminalId) => {
  const term = mountTerminal(container, geometry);
  /** W5: the single pane's terminal is the one a worker's window measures its
   * cell size from. A split column is never measured. */
  const inPane = container.closest(".fd-pane") !== null;
  if (inPane) {
    paneTerminal = term;
    scheduleGeometrySync();
  }
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
  return {
    dispose: () => {
      socket?.detachTerminal(terminalId);
      if (paneTerminal === term) {
        paneTerminal = null;
      }
      term.dispose();
    },
    /**
     * W5, a worker's window only (`ui/terminalStage.ts` calls it nowhere
     * else): the worker sized this terminal's PTY to the pane, so xterm takes
     * the same grid in place instead of being rebuilt.
     */
    resize: (next) => {
      term.resize(next.cols, next.rows);
      scheduleGeometrySync();
    },
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
      /**
       * Takeover has no frame of its own: the browser re-sends `Attach` (2f).
       *
       * Both answers travel now. Under D14 as revised a refused keystroke costs
       * the *turn* and not the seat, so the host does not move this browser
       * anywhere on its own — `Take over` has to say so, and so does `Watch
       * read-only`, or the panel would close over a seat nothing changed.
       */
      case "takeover/claim":
        session?.requestSeat("take_over");
        return;
      case "takeover/observe":
        session?.requestSeat("observe");
        return;
      /**
       * A worker's window (`remote-control-vjtb`). The picker and the shell's
       * Restart button report what they want; these are the worker commands
       * that means, sent here and nowhere else.
       */
      case "picker/request":
        runPickerIntent(action.intent);
        return;
      case "workerShell/restart":
        session.sendCommand(WORKER_COMMAND.RESTART_WORKER_SHELL);
        return;
      /** Uploading files to a worker (v8, R30): the requests the panel's
       * buttons mean, and Cancel's abort. */
      case "upload/submit":
        submitUpload();
        return;
      case "upload/resolve":
        resolveUpload(action.resolution);
        return;
      case "upload/cancel":
        abortTransfer();
        return;
      /** The Transfer files panel's Download tab (v9): its listings and its
       * `begin_download`. The tickets are handed to the browser by
       * `pumpDownloads`, below. */
      case "transfer/open":
        listDownloadFolder();
        return;
      case "download/intent":
        runDownloadIntent(action.intent);
        return;
      case "download/startOver":
        /** The same folder again, freshly listed: what was just downloaded
         * may have changed since. */
        runDownloadIntent({ kind: "open" });
        return;
      case "download/submit":
        submitDownload();
        return;
      default:
        return;
    }
  },
  onEnableNotifications: () => {
    void askNotificationPermission();
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
   * 1f, `remote-control-ll5.6` / `remote-control-1p22`. `s` sends the staged
   * edits as the `args` of an `open_configuration` frame — the same name the
   * palette row sends, because the answer to a save *is* the manager: the host
   * applies the edits, writes the two files and replies with the re-resolved
   * layering, which `socket.ts` turns into `config/received` (§6.5 R22).
   *
   * Same seam as `onRunCommand`: the transport mints the seq, and
   * `command/result` reports whatever really came back — never a fabricated
   * success.
   */
  onSaveConfig: (request) => {
    if (session === null) {
      return;
    }
    const seq = session.sendCommand(OPEN_CONFIGURATION_COMMAND, request);
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
       * the SPA ships inside the binary (D9) — so the fix really is a reload.
       * This only ever runs from a click or from `Enter` while the chip
       * itself has focus (`ui/statusBar.ts`'s `actionButton`) — never from a
       * global binding. See `specs/WEB_INTERFACE.md` §6.5 R9. */
      window.location.reload();
      return;
    }
    if (action.kind === "code") {
      app.store.dispatch({
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
    /**
     * `reclaim` — a worker's "Take it back" (W17): attach as its window
     * again, which takes it from the window that took it from us.
     */
    if (action.kind === "reclaim") {
      session?.reconnect();
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

/**
 * 1h's breakpoint, measured here and decided in the reducer
 * (`remote-control-eek.4`, `specs/WEB_INTERFACE.md` §6.5 R17).
 *
 * This is the **only** place in the app that asks how wide anything is, and it
 * is this file for the same reason `window.location`, the cookie exchange and
 * the socket are: `main.ts` owns the impure edges, and `state/` and `ui/` stay
 * testable without a browser. What goes on the action is the pixel width, not
 * the layout — `widthClass` turns one into the other, so the 900px boundary is
 * a unit test rather than a media query jsdom will never evaluate.
 * `tokens.guard.test.ts` rule 5 fails the build if a second place starts
 * measuring, or if a width media query appears in `src/style/`.
 *
 * Dispatched once before anything is painted, then on every `resize`. There is
 * no debounce: `reduce` returns the *same state object* when the class has not
 * changed (the overwhelming majority of resize events), so the store's identity
 * check means a drag across a window edge re-renders exactly twice — once at
 * each crossing.
 */
function measureViewport(): void {
  app.store.dispatch({
    type: "viewport/measured",
    pixels: window.innerWidth,
  });
}
measureViewport();
window.addEventListener("resize", measureViewport);

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
    /** The refusal now carries `revoked_at_ms` beside the host's own
     * `server_time_ms`, so 2b's "withdrew this browser's access **12s ago**"
     * can render its time. A host that did not send them leaves `revokedAgo`
     * `null`, and the sentence prints without the clause rather than with an
     * invented or zero time. */
    store.dispatch({
      type: "access/revoked",
      revokedAgo: result.revokedAgo,
      hostKind: result.hostKind,
    });
    return;
  }
  store.dispatch({
    type: after === "exchange" ? "access/refused" : "access/required",
    screen: result.screen,
    /** Every number here is the host's, straight from the refusal body. */
    attemptsRemaining: result.attemptsRemaining,
    lockoutSeconds: result.lockoutSeconds,
    lockoutLengthSeconds: result.lockoutLengthSeconds,
    codeTtlSeconds: result.codeTtlSeconds,
    /** The refusal says which host refused us, so the screen can say where a
     * code comes from (`remote-control-vjtb`). */
    hostKind: result.hostKind,
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
  /** The terminal on screen — the selection's on every host but a worker,
   * whose shell tab is its own terminal. */
  const terminalId = onScreenTerminalId(state);
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
    onNotification: (notification) => showNotification(notification),
  });
}

// ---------------------------------------------------------------------------
// A FlightDeck Worker's window (`remote-control-vjtb`)
// ---------------------------------------------------------------------------

/**
 * One picker intent → the worker command it means, sent, and its seq handed
 * back to the picker so it can tell its answers from anyone else's.
 */
function runPickerIntent(intent: PickerIntent): void {
  const picker = app.store.getState().picker;
  if (session === null || picker === null) {
    return;
  }
  const send = (request: PickerRequest): void => {
    if (session === null) {
      return;
    }
    const seq = session.sendCommand(request.name, request.args);
    app.store.dispatch({ type: "picker/sent", sent: request.sent, seq });
  };
  switch (intent.kind) {
    case "open":
      /** Scan the projects root and list the folder the picker is in — `$HOME`
       * the first time (`session.rs`'s `open_picker`). */
      send(scanRequest());
      send(browseRequest(picker, picker.cwd));
      return;
    case "scan":
      send(scanRequest());
      return;
    case "browse":
      send(browseRequest(picker, intent.path));
      return;
    case "up":
      if (picker.parent !== null) {
        send(browseRequest(picker, picker.parent));
      }
      return;
    case "toggle_hidden":
      app.store.dispatch({ type: "picker/toggleHidden" });
      {
        const toggled = app.store.getState().picker;
        if (toggled !== null) {
          send(browseRequest(toggled, toggled.cwd));
        }
      }
      return;
    case "open_path":
      send(openRequest(intent.path));
      return;
    case "clone": {
      const request = cloneRequest(picker);
      if ("error" in request) {
        app.store.dispatch({ type: "picker/error", error: request.error });
        return;
      }
      send(request);
      return;
    }
    case "choose_folder":
      /** The upload panel's Choose…: list the panel's folder, nothing else. */
      send(browseRequest(picker, intent.path));
      return;
  }
}

// ---------------------------------------------------------------------------
// Uploading files to a worker (v8, W34, R30)
// ---------------------------------------------------------------------------

/**
 * Send a `begin_upload` and hand its seq back to the panel — `null` when the
 * link is down, since a command sent then is never answered.
 */
function sendUploadRequest(request: UploadRequest): void {
  const state = app.store.getState();
  if (session === null || state.connection !== "connected") {
    app.store.dispatch({
      type: "upload/requested",
      seq: null,
      onConflict: request.onConflict,
    });
    return;
  }
  const command = beginUploadCommand(request);
  const seq = session.sendCommand(command.name, command.args);
  app.store.dispatch({ type: "upload/requested", seq, onConflict: request.onConflict });
}

/** Upload pressed: what the list and the folder mean, or why nothing. */
function submitUpload(): void {
  const upload = app.store.getState().upload;
  if (upload === null) {
    return;
  }
  const request = uploadRequest(upload);
  if (request === null) {
    return;
  }
  if ("note" in request) {
    app.store.dispatch({ type: "upload/note", note: request.note });
    return;
  }
  sendUploadRequest(request);
}

/** Replace or Keep both: the same files again, with that answer. */
function resolveUpload(resolution: Resolution): void {
  const upload = app.store.getState().upload;
  if (upload === null || resolution === "cancel") {
    return;
  }
  const { request } = uploadResolve(upload, resolution);
  if (request !== null) {
    sendUploadRequest(request);
  }
}

/**
 * The one transfer under way, if any. Files go one at a time, in list order,
 * right after the plan: each ticket lapses two minutes after it was granted.
 */
let transfer: UploadTransfer | null = null;
let pumpScheduled = false;

function abortTransfer(): void {
  const running = transfer;
  transfer = null;
  running?.abort();
}

/** Start the next file when the panel is uploading and nothing is sending.
 * Deferred to a microtask so it never dispatches from inside the store's own
 * notification pass. */
function pumpUploads(): void {
  if (pumpScheduled || transfer !== null) {
    return;
  }
  const upload = app.store.getState().upload;
  if (upload === null || nextUploadJob(upload) === null) {
    return;
  }
  pumpScheduled = true;
  queueMicrotask(() => {
    pumpScheduled = false;
    const current = app.store.getState().upload;
    const job = current === null ? null : nextUploadJob(current);
    if (job === null || transfer !== null) {
      return;
    }
    app.store.dispatch({ type: "upload/started", index: job.index });
    /** The slot is `transfer` before the request exists, so an answer can
     * never arrive for a transfer this file does not know about yet. */
    let inner: UploadTransfer | null = null;
    const mine: UploadTransfer = { abort: () => inner?.abort() };
    transfer = mine;
    inner = putUpload({
      ticket: job.ticket,
      body: job.source,
      onProgress: (sent) => {
        if (transfer === mine) {
          app.store.dispatch({ type: "upload/progress", index: job.index, sent });
        }
      },
      onDone: (result) => {
        if (transfer !== mine) {
          return; /** Cancelled: the panel has moved on already. */
        }
        transfer = null;
        app.store.dispatch({ type: "upload/finished", index: job.index, result });
      },
    });
  });
}
app.store.subscribe(pumpUploads);

// ---------------------------------------------------------------------------
// Downloading files from a worker (v9)
// ---------------------------------------------------------------------------

/** The Download tab is showing with nothing listed and nothing asked: list
 * its starting folder. */
function listDownloadFolder(): void {
  const state = app.store.getState();
  const download = state.download;
  if (
    state.upload === null ||
    state.transferTab !== "download" ||
    download === null ||
    download.cwd !== null ||
    download.browse !== null
  ) {
    return;
  }
  runDownloadIntent({ kind: "open" });
}

/** One Download-tab intent → the `browse_dir` it means, sent, and its seq
 * handed back so the tab can tell its listing from the picker's. */
function runDownloadIntent(intent: DownloadIntent): void {
  const download = app.store.getState().download;
  if (download === null) {
    return;
  }
  const path = downloadBrowseTarget(download, intent);
  if (path === undefined) {
    return;
  }
  if (session === null || app.store.getState().connection !== "connected") {
    app.store.dispatch({ type: "download/browsing", seq: null });
    return;
  }
  const command = downloadBrowseCommand(download, path);
  const seq = session.sendCommand(command.name, command.args);
  app.store.dispatch({ type: "download/browsing", seq });
}

/** Download pressed: the folder and the ticked files, or why nothing. */
function submitDownload(): void {
  const download = app.store.getState().download;
  if (download === null) {
    return;
  }
  const request = downloadRequest(download);
  if (request === null) {
    return;
  }
  if ("note" in request) {
    app.store.dispatch({ type: "download/note", note: request.note });
    return;
  }
  if (session === null || app.store.getState().connection !== "connected") {
    app.store.dispatch({ type: "download/requested", seq: null });
    return;
  }
  const command = beginDownloadCommand(request);
  const seq = session.sendCommand(command.name, command.args);
  app.store.dispatch({ type: "download/requested", seq });
}

/**
 * Hand granted files to the browser, once: marked handed off first, so the
 * notification this dispatch causes cannot start them twice. Deferred to a
 * microtask so it never dispatches from inside the store's own notification
 * pass.
 */
let handoffScheduled = false;
function pumpDownloads(): void {
  if (handoffScheduled) {
    return;
  }
  const download = app.store.getState().download;
  if (download === null || nextDownloadHandoff(download) === null) {
    return;
  }
  handoffScheduled = true;
  queueMicrotask(() => {
    handoffScheduled = false;
    const current = app.store.getState().download;
    const handoff = current === null ? null : nextDownloadHandoff(current);
    if (handoff === null) {
      return;
    }
    app.store.dispatch({ type: "download/handedOff" });
    startDownloads({ files: handoff.files });
  });
}
app.store.subscribe(pumpDownloads);

/**
 * Go to the project the picker had the worker open, once it shows up in a
 * snapshot. Selection is shared (D3), so this is the same `select_project` a
 * click on its tab sends — with one window on a worker, that is this window's
 * own view moving.
 */
let selectingOpened = false;
function selectOpenedProject(): void {
  const state = app.store.getState();
  const opened = state.pendingOpen;
  if (opened === null || selectingOpened || state.hostKind !== "worker") {
    return;
  }
  const project = state.projects.find(
    (candidate) =>
      (opened.projectId !== null && candidate.id === opened.projectId) ||
      candidate.root === opened.path,
  );
  if (project === undefined) {
    return;
  }
  selectingOpened = true;
  queueMicrotask(() => {
    selectingOpened = false;
    app.store.dispatch({ type: "pendingOpen/done" });
    app.store.dispatch({ type: "selection/project", projectId: project.id });
  });
}
app.store.subscribe(selectOpenedProject);

/**
 * W5: a worker has no pane of its own, so this window's pane is the size of
 * the terminal on screen. Measured here — the one file allowed to ask how big
 * anything is (`tokens.guard.test.ts` rule 5) — and sent for that terminal
 * whenever it differs from the last size sent for it (the transport keeps that
 * record per connection). Re-measured when the pane resizes, when the terminal
 * on screen changes (a tab switch), and when a terminal mounts.
 *
 * Every other host owns its geometry (D4): `resizeTerminal` refuses to send to
 * one, and nothing here runs until the snapshot says `worker`.
 */
let geometryFrame: number | null = null;
let lastOnScreen: string | null = null;
let lastConnection: string | null = null;
function scheduleGeometrySync(): void {
  if (geometryFrame !== null) {
    return;
  }
  geometryFrame = requestAnimationFrame(() => {
    geometryFrame = null;
    syncWorkerGeometry();
  });
}

function syncWorkerGeometry(): void {
  const state = app.store.getState();
  if (session === null || state.hostKind !== "worker") {
    return;
  }
  const terminalId = onScreenTerminalId(state);
  const term = paneTerminal;
  if (terminalId === null || term === null || term.element === undefined) {
    return;
  }
  const stage = app.el.querySelector<HTMLElement>(".fd-pane .fd-stage");
  const letterbox = stage?.querySelector<HTMLElement>(".fd-letterbox") ?? null;
  const screen = term.element.querySelector<HTMLElement>(".xterm-screen");
  if (stage === null || stage === undefined || letterbox === null || screen === null) {
    return;
  }
  const stageBox = stage.getBoundingClientRect();
  const screenBox = screen.getBoundingClientRect();
  const termBox = term.element.getBoundingClientRect();
  const pad = getComputedStyle(letterbox);
  /** What surrounds the grid: the letterbox's padding, and whatever xterm
   * draws beside its screen (a scrollbar gutter, say). */
  const chromeWidth =
    parseFloat(pad.paddingLeft) +
    parseFloat(pad.paddingRight) +
    Math.max(0, termBox.width - screenBox.width);
  const chromeHeight =
    parseFloat(pad.paddingTop) +
    parseFloat(pad.paddingBottom) +
    Math.max(0, termBox.height - screenBox.height);
  const cells = cellsFor(
    {
      width: stageBox.width - chromeWidth,
      height: stageBox.height - chromeHeight,
    },
    { width: screenBox.width / term.cols, height: screenBox.height / term.rows },
  );
  if (cells !== null) {
    session.resizeTerminal(terminalId, cells.cols, cells.rows);
  }
}

/** The pane resizing — a window edge, the sidebar sliding, the shell's tab
 * going full width — is a new size for the terminal in it. */
if (typeof ResizeObserver !== "undefined") {
  const observer = new ResizeObserver(() => scheduleGeometrySync());
  const stage = app.el.querySelector<HTMLElement>(".fd-pane .fd-stage");
  if (stage !== null) {
    observer.observe(stage);
  }
}
window.addEventListener("resize", scheduleGeometrySync);
/** A tab switch, a reattach (whose sizes the transport has forgotten), or the
 * host turning out to be a worker. */
app.store.subscribe((state) => {
  if (state.hostKind !== "worker") {
    return;
  }
  const onScreen = onScreenTerminalId(state);
  if (onScreen !== lastOnScreen || state.connection !== lastConnection) {
    lastOnScreen = onScreen;
    lastConnection = state.connection;
    scheduleGeometrySync();
  }
});

/**
 * W18: what this browser lets the page do about notifications. Browsers
 * refuse the API outside a secure context, and plain `http://` to a worker on
 * a tailnet address is exactly that — so there the status bar offers nothing,
 * rather than a button that cannot work.
 */
function notificationPermission(): NotificationPermissionState {
  if (!window.isSecureContext || typeof Notification === "undefined") {
    return "unsupported";
  }
  return Notification.permission;
}
app.store.dispatch({
  type: "notifications/permission",
  permission: notificationPermission(),
});

async function askNotificationPermission(): Promise<void> {
  if (notificationPermission() === "unsupported") {
    return;
  }
  try {
    await Notification.requestPermission();
  } catch {
    /** An old Safari answers by callback only; the read below still tells. */
  }
  app.store.dispatch({
    type: "notifications/permission",
    permission: notificationPermission(),
  });
}

/**
 * A worker's notification (W18), raised by this browser only when it would
 * otherwise be missed: the page is hidden, the person said yes, and the host
 * is a worker. A visible page already shows what the notification is about.
 */
function showNotification(notification: WireNotificationView): void {
  if (
    app.store.getState().hostKind !== "worker" ||
    notificationPermission() !== "granted" ||
    !document.hidden
  ) {
    return;
  }
  try {
    new Notification(notification.title, {
      body: notification.body,
      silent: notification.sound === "none",
      tag: `flightdeck-worker-${notification.at_ms}`,
    });
  } catch (error) {
    console.warn("[notify] could not raise a notification:", error);
  }
}
