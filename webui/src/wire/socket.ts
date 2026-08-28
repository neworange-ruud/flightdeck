/**
 * The live session: protocol v1 over `GET /ws` (D12).
 *
 * This is the half of the app `src/main.ts` used to fake. Everything the screen
 * shows now comes from the host: the snapshot paints the tree, `term_bytes`
 * carries raw PTY output straight into xterm.js, and a keystroke goes back as an
 * `input` frame. The components never learn the difference — they are handed the
 * same actions the fixture used to dispatch.
 *
 * Three properties worth stating, because they are the ones that break quietly:
 *
 * 1. **Bytes never enter the store.** `term_bytes` goes to the mounted
 *    terminal's sink. The store holds structure, xterm.js holds the screen, and
 *    a re-render therefore cannot repaint (or lose) terminal output.
 * 2. **Bytes are bytes.** They are base64-decoded to a `Uint8Array` and written
 *    as such, so a UTF-8 sequence split across two frames is xterm's problem to
 *    reassemble — which it is equipped for and a JS string is not.
 * 3. **Attach is the handshake.** The host closes the socket on any frame sent
 *    before `attach`, so nothing is written to the wire until the socket is open
 *    and `attach` has gone out; keystrokes typed before that are queued.
 */

import type { ShutdownReason } from "../state/model";
import type { Store } from "../ui/store";
import { dialogOf, seatOf, snapshotFromWire, statusFromLabel } from "./adapt";
import {
  decodeBase64,
  encodeBase64,
  PROTOCOL_VERSION,
  WS_PATH,
  type ServerFrame,
  type WireAck,
  type WireDeltaEnvelope,
  type WireDialogView,
  type WireError,
  type WireGeometry,
  type WireSeatInfo,
  type WireShutdown,
  type WireSnapshot,
  type WireTermBytes,
} from "./frames";

/** Where a mounted terminal receives its bytes. */
export type TerminalSink = (bytes: Uint8Array) => void;

/**
 * Per-terminal bytes kept locally so a remount can repaint.
 *
 * The host replays from *its* ring only on a new connection; a remount inside
 * one connection (xterm is rebuilt when the host's grid changes, D4) would
 * otherwise come up blank. Bounded, and bounded in bytes rather than lines
 * because that is the unit the buffer holds.
 */
const LOCAL_SCROLLBACK_BYTES = 256 * 1024;

/** Reconnect backoff, capped. Deliberately short at first — a host restart is
 * usually over in a second and the tab should not sit there looking dead. */
const RETRY_DELAYS_MS = [250, 500, 1000, 2000, 4000, 8000];

interface QueuedInput {
  readonly seq: number;
  readonly terminalId: string;
  readonly data: Uint8Array;
}

export interface SessionSocket {
  /** Register the sink for a mounted terminal; replays what we already hold. */
  attachTerminal(terminalId: string, sink: TerminalSink): void;
  detachTerminal(terminalId: string): void;
  /** A keystroke for `terminalId`. Queued when the link is down (§5.1). */
  sendInput(terminalId: string, data: string): void;
  /**
   * A named command (`protocol::command`). Returns the seq assigned — the
   * only place that number is minted, since it shares the counter with
   * `Input` frames (§5.1) — so a caller that wants to know how the command
   * turned out (the palette, `ll5.2`) can match it against a later
   * `command/result`.
   */
  sendCommand(name: string, args?: unknown): number;
  close(): void;
}

export interface SessionSocketOptions {
  readonly store: Store;
  /** Same-origin by default; the cookie rides along because it is `HttpOnly`. */
  readonly url?: string;
  /** The browser's viewport in cells, for the letterbox only — never a PTY. */
  readonly viewport?: () => WireGeometry | null;
  /** Injected in tests. */
  readonly socketFactory?: (url: string) => WebSocket;
  readonly now?: () => number;
}

function defaultUrl(): string {
  const scheme = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${window.location.host}${WS_PATH}`;
}

const SHUTDOWN_REASONS: readonly ShutdownReason[] = [
  "host_quit",
  "server_stopped",
  "token_revoked",
  "restarting",
];

function shutdownReason(reason: string): ShutdownReason {
  return (SHUTDOWN_REASONS as readonly string[]).includes(reason)
    ? (reason as ShutdownReason)
    : "unknown";
}

/** Open the session and keep it open. Returns immediately; the first frames
 * arrive asynchronously. */
export function openSession(options: SessionSocketOptions): SessionSocket {
  const { store } = options;
  const url = options.url ?? defaultUrl();
  const factory = options.socketFactory ?? ((u: string) => new WebSocket(u));
  const now = options.now ?? (() => Date.now());

  const sinks = new Map<string, TerminalSink>();
  const scrollback = new Map<string, Uint8Array>();
  /** Per-terminal byte cursor: the offset of the next byte we have not seen. */
  const cursors = new Map<string, number>();
  /** Keystrokes not yet known to be applied. Never dropped, never reordered. */
  let held: QueuedInput[] = [];
  /**
   * Seqs `sendCommand` minted that have not been resolved yet. `seq` is one
   * monotonic counter shared with `Input` frames (§5.1), so an `Ack`/`Error`
   * for a command seq must not fall through to the input-ack bookkeeping
   * below it — this set is what tells the two apart.
   */
  const pendingCommands = new Set<number>();
  let seq = 0;
  let viewerId: string | null = null;
  let socket: WebSocket | null = null;
  let attached = false;
  let closedForGood = false;
  let attempt = 0;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  let snapshotTimer: ReturnType<typeof setTimeout> | null = null;

  function remember(terminalId: string, bytes: Uint8Array): void {
    const previous = scrollback.get(terminalId);
    const merged =
      previous === undefined
        ? bytes
        : (() => {
            const joined = new Uint8Array(previous.length + bytes.length);
            joined.set(previous, 0);
            joined.set(bytes, previous.length);
            return joined;
          })();
    scrollback.set(
      terminalId,
      merged.length > LOCAL_SCROLLBACK_BYTES
        ? merged.subarray(merged.length - LOCAL_SCROLLBACK_BYTES)
        : merged,
    );
  }

  function send(frame: unknown): boolean {
    if (socket === null || socket.readyState !== WebSocket.OPEN || !attached) {
      return false;
    }
    socket.send(JSON.stringify(frame));
    return true;
  }

  function flushHeld(): void {
    for (const item of held) {
      send({
        type: "input",
        seq: item.seq,
        terminal_id: item.terminalId,
        data: encodeBase64(item.data),
      });
    }
  }

  /**
   * Ask for a fresh snapshot, coalesced.
   *
   * Several delta kinds (status, git, a new session) change facts the store only
   * accepts as part of a whole snapshot, and `request_snapshot` is the host's
   * own answer for a browser that believes it has drifted. Coalescing keeps a
   * burst of transitions from turning into a burst of snapshots.
   */
  function requestSnapshotSoon(): void {
    if (snapshotTimer !== null) {
      return;
    }
    snapshotTimer = setTimeout(() => {
      snapshotTimer = null;
      sendCommand("request_snapshot");
    }, 150);
  }

  function onSnapshot(frame: WireSnapshot): void {
    if (frame.protocol_version !== PROTOCOL_VERSION) {
      /** A stale tab, not a negotiation: the SPA ships inside the binary (D9). */
      store.dispatch({
        type: "version/mismatch",
        mismatch: {
          tabVersion: `protocol v${PROTOCOL_VERSION}`,
          hostVersion: `protocol v${frame.protocol_version}`,
        },
      });
      return;
    }
    viewerId = frame.viewer_id;
    /** Everything the host has already applied leaves the held queue (§5.1). */
    held = held.filter((item) => item.seq > frame.last_input_seq);
    seq = Math.max(seq, frame.last_input_seq);
    store.dispatch({ type: "snapshot/received", snapshot: snapshotFromWire(frame) });
    store.dispatch({ type: "connection/changed", status: "connected" });
    store.dispatch({ type: "retry/set", retry: null });
    /** 1a is drawn in Terminal mode, and a browser that just attached is here
     * to watch a terminal. */
    store.dispatch({ type: "mode/set", mode: "terminal" });
    flushHeld();
  }

  function onTermBytes(frame: WireTermBytes): void {
    const bytes = decodeBase64(frame.data);
    cursors.set(frame.terminal_id, frame.offset + bytes.length);
    if (frame.truncated === true) {
      store.dispatch({
        type: "replay/set",
        replay: {
          bytesDone: bytes.length,
          bytesTotal: bytes.length,
          fromByte: frame.offset,
          truncated: true,
        },
      });
    }
    remember(frame.terminal_id, bytes);
    sinks.get(frame.terminal_id)?.(bytes);
  }

  function onDelta(frame: WireDeltaEnvelope): void {
    switch (frame.change) {
      case "seats": {
        const seats =
          (frame.seats as readonly WireSeatInfo[] | undefined) ?? [];
        /**
         * The host's own clock, sent beside the rows so `since_ms` can be dated
         * without asking this machine what time it is. A host from before the
         * field sends nothing (or a `0`, which is serde's default and not a
         * time) — `null` then, and the rows come back undated rather than dated
         * against a clock that has no relationship to the host's.
         */
        const serverTimeMs =
          typeof frame.server_time_ms === "number" && frame.server_time_ms > 0
            ? frame.server_time_ms
            : null;
        store.dispatch({
          type: "seats/changed",
          seat: (frame.you as "controlling" | "observing") ?? "observing",
          /** The same mapping the snapshot path uses, deliberately: 2f draws
           * the same three facts however the seat news arrived. */
          seats: seats.map((s) => seatOf(s, serverTimeMs)),
        });
        return;
      }
      case "geometry": {
        const geometry = frame.geometry as WireGeometry | undefined;
        if (geometry !== undefined) {
          store.dispatch({ type: "geometry/set", geometry });
        }
        return;
      }
      case "selection": {
        /**
         * `Delta::Selection` (D3/D8, `remote-control-ll5.7`): the instance's
         * shared selection moved, possibly because *this* browser's own
         * `toggle_split_view` command was applied, possibly because the
         * desktop (or another browser) moved it. `split_view` is applied
         * immediately and directly — it is the host's own word on the
         * matter, not a guess — which is what lets a toggle's effect show up
         * without waiting on the coalesced resync below.
         *
         * The rest of the envelope (`project_id`/`session_id`/`terminal_id`)
         * is intentionally *not* applied field-by-field here: unlike
         * `layout`, `AppState.selection` has no "apply this one field" action
         * for a wire delta, only the browser-initiated `selection/*` actions
         * (D3, `main.ts`). `requestSnapshotSoon` — the same fallback every
         * other unhandled delta already takes — reconciles it via a whole
         * `Snapshot`, so a session/terminal change made elsewhere still
         * lands, just one coalesced round trip later rather than never.
         */
        const splitView = frame.split_view as boolean | undefined;
        if (splitView !== undefined) {
          store.dispatch({
            type: "layout/set",
            layout: splitView ? "split" : "single",
          });
        }
        requestSnapshotSoon();
        return;
      }
      /**
       * D13: the dialog is app state, so both halves of its life are applied
       * directly rather than resynced. `requestSnapshotSoon` would work, but it
       * would put a coalesced round trip between a `y` pressed on the desktop
       * and the modal leaving this screen — and the frame already carries
       * everything the store needs.
       */
      case "dialog_opened": {
        const view = frame as unknown as WireDialogView;
        store.dispatch({ type: "dialog/opened", dialog: dialogOf(view) });
        return;
      }
      case "dialog_closed": {
        const closed = frame as unknown as {
          dialog_id: string;
          outcome: "confirmed" | "cancelled" | "superseded";
        };
        store.dispatch({
          type: "dialog/closed",
          dialogId: closed.dialog_id,
          /**
           * `superseded` is the host saying "replaced without a decision", and
           * it is passed through rather than flattened into a cancel: a browser
           * that reported it as cancelled would be claiming somebody answered a
           * question nobody answered.
           */
          outcome: closed.outcome,
        });
        return;
      }
      case "activity": {
        /**
         * `Delta::Activity` flattens the same `protocol::ActivityEvent` the
         * snapshot's backfill carries (`Delta` is internally tagged on
         * `change`, and `Activity` is a newtype variant around the struct) —
         * so `from`/`to` are real `InterpretedStatus` labels here, not a
         * placeholder. Mapping them with `statusFromLabel`, the same function
         * `wire/adapt.ts`'s `activityOf` uses for the backfill, is what makes
         * "unknown stays unknown" one rule instead of two: a *genuinely*
         * unknown-lifecycle event still renders `unknown → unknown`, because
         * that is what `statusFromLabel("unknown", "unknown")` resolves to —
         * it is no longer merely what every live row said regardless of what
         * the host sent.
         */
        const event = frame as unknown as {
          event_id: string;
          project_id: string;
          project_name: string;
          session_id: string;
          session_name: string;
          from: string;
          to: string;
          reason?: string;
          tier: "attention" | "finished" | "quiet";
          read?: boolean;
        };
        store.dispatch({
          type: "activity/received",
          events: [
            {
              id: event.event_id,
              /** A delta just happened; there is no clock-skew-free way to
               * turn `at_ms` into "Nm ago" better than the honest present
               * tense the backfill's `agoLabel` would eventually relax to
               * anyway. */
              atLabel: "just now",
              projectId: event.project_id,
              projectName: event.project_name,
              sessionId: event.session_id,
              sessionName: event.session_name,
              from: statusFromLabel(event.from, "unknown"),
              to: statusFromLabel(event.to, "unknown"),
              reason: event.reason ?? "",
              tier: event.tier,
              read: event.read ?? false,
            },
          ],
        });
        return;
      }
      default:
        /** Every other change alters facts the store only takes wholesale, and
         * an unknown `change` from a newer host lands here too — asking for a
         * snapshot is correct for both. */
        requestSnapshotSoon();
    }
  }

  function onError(frame: WireError): void {
    /**
     * D14: an observer's `select_*` is *refused*, not Ack'd — `ServerMsg::Error
     * { code: "read_only" }`, `seq` naming the command it refused (D14, and
     * `tests/web_server.rs`'s `an_observers_command_is_refused_as_read_only`).
     * Folded into the same `command/result` action an `Ack` would produce, so
     * the palette has one place to look for "what happened", not two.
     */
    if (
      frame.code === "read_only" &&
      frame.seq !== undefined &&
      pendingCommands.has(frame.seq)
    ) {
      pendingCommands.delete(frame.seq);
      store.dispatch({
        type: "command/result",
        seq: frame.seq,
        outcome: "read_only",
        detail: frame.message,
      });
      return;
    }
    if (frame.code === "version_mismatch") {
      store.dispatch({
        type: "version/mismatch",
        mismatch: {
          tabVersion: `protocol v${frame.version?.peer ?? PROTOCOL_VERSION}`,
          hostVersion: `protocol v${frame.version?.max_supported ?? "?"}`,
        },
      });
      closedForGood = true;
      return;
    }
    if (frame.code === "seat_held") {
      /** D14: someone else is driving. Watch read-only and offer the takeover,
       * rather than sitting on a socket with no seat. */
      const incumbent = frame.incumbent;
      store.dispatch({
        type: "takeover/held",
        incumbent: {
          /**
           * 2f's three rows, each from its own field. The label is the
           * fallback for the address only — it *starts* with the address the
           * host observed — and it is never split to fill the browser row,
           * because the half after the separator is a user-agent string that
           * may contain another separator. An unknown browser leaves the row
           * empty and `factList` drops it.
           */
          address: incumbent?.address ?? incumbent?.label ?? "another browser",
          browser: incumbent?.user_agent_label ?? "",
          connected: "",
        },
      });
      sendAttach("observe");
      return;
    }
    console.warn(`[ws] ${frame.code}: ${frame.message}`);
  }

  function onAck(frame: WireAck): void {
    if (pendingCommands.has(frame.seq)) {
      pendingCommands.delete(frame.seq);
      store.dispatch({
        type: "command/result",
        seq: frame.seq,
        outcome: frame.outcome,
        ...(frame.detail === undefined ? {} : { detail: frame.detail }),
      });
      return;
    }
    if (frame.outcome === "applied") {
      held = held.filter((item) => item.seq > frame.seq);
      store.dispatch({ type: "input/acked", throughSeq: frame.seq });
      return;
    }
    /** `ignored` means the host is already past it; `rejected` means it will
     * never land. Either way it must leave the queue, or it is retried for
     * ever on the next reconnect. */
    held = held.filter((item) => item.seq !== frame.seq);
    if (frame.detail !== undefined) {
      console.info(`[ws] input ${frame.seq} ${frame.outcome}: ${frame.detail}`);
    }
  }

  function onShutdown(frame: WireShutdown): void {
    const reason = shutdownReason(frame.reason);
    /** Q5: a deliberate quit is a terminal state, not a network failure. Only
     * `restarting` is worth waiting for. */
    closedForGood = reason !== "restarting";
    store.dispatch({
      type: "connection/shutdown",
      shutdown: {
        reason,
        selfInitiated: frame.self_initiated ?? false,
        detail: frame.detail ?? "",
        atLabel: new Date(now()).toLocaleTimeString(),
      },
    });
  }

  function handle(frame: ServerFrame): void {
    switch (frame.type) {
      case "snapshot":
        onSnapshot(frame as WireSnapshot);
        return;
      case "term_bytes":
        onTermBytes(frame as WireTermBytes);
        return;
      case "delta":
        onDelta(frame as WireDeltaEnvelope);
        return;
      case "ack":
        onAck(frame as WireAck);
        return;
      case "error":
        onError(frame as WireError);
        return;
      case "shutdown":
        onShutdown(frame as WireShutdown);
        return;
      default:
        /** A frame this build has never heard of. Ignored on purpose. */
        return;
    }
  }

  function sendAttach(seat: "control" | "take_over" | "observe"): void {
    if (socket === null || socket.readyState !== WebSocket.OPEN) {
      return;
    }
    /** `attached` gates every *other* frame, so it has to be true for the
     * attach itself to go out. */
    attached = true;
    socket.send(
      JSON.stringify({
        type: "attach",
        protocol_version: PROTOCOL_VERSION,
        seat,
        cursors: [...cursors.entries()].map(([terminal_id, next_offset]) => ({
          terminal_id,
          next_offset,
        })),
        resume_viewer: viewerId,
        viewport: options.viewport?.() ?? null,
        client: { user_agent: navigator.userAgent },
      }),
    );
  }

  function connect(): void {
    if (closedForGood) {
      return;
    }
    store.dispatch({
      type: "connection/changed",
      status: attempt === 0 ? "connecting" : "reconnecting",
    });
    let ws: WebSocket;
    try {
      ws = factory(url);
    } catch (error) {
      scheduleRetry();
      console.warn("[ws] could not open the socket:", error);
      return;
    }
    socket = ws;
    attached = false;
    ws.onopen = () => {
      attempt = 0;
      sendAttach("control");
    };
    ws.onmessage = (event: MessageEvent) => {
      if (typeof event.data !== "string") {
        return; /** v1 is JSON over text frames. */
      }
      let frame: ServerFrame;
      try {
        frame = JSON.parse(event.data) as ServerFrame;
      } catch {
        console.warn("[ws] dropped a frame that was not JSON");
        return;
      }
      handle(frame);
    };
    ws.onclose = () => {
      socket = null;
      attached = false;
      if (closedForGood) {
        return;
      }
      scheduleRetry();
    };
    ws.onerror = () => {
      /** `onclose` always follows, and it owns the retry. */
    };
  }

  function scheduleRetry(): void {
    if (retryTimer !== null || closedForGood) {
      return;
    }
    const delay =
      RETRY_DELAYS_MS[Math.min(attempt, RETRY_DELAYS_MS.length - 1)] ?? 8000;
    attempt += 1;
    store.dispatch({ type: "connection/changed", status: "reconnecting" });
    store.dispatch({
      type: "retry/set",
      retry: { attempt, inSeconds: Math.round(delay / 1000) },
    });
    retryTimer = setTimeout(() => {
      retryTimer = null;
      connect();
    }, delay);
  }

  function sendCommand(name: string, args?: unknown): number {
    seq += 1;
    const mySeq = seq;
    pendingCommands.add(mySeq);
    send(args === undefined
      ? { type: "command", seq: mySeq, name }
      : { type: "command", seq: mySeq, name, args });
    return mySeq;
  }

  connect();

  return {
    attachTerminal(terminalId, sink) {
      sinks.set(terminalId, sink);
      const buffered = scrollback.get(terminalId);
      if (buffered !== undefined && buffered.length > 0) {
        sink(buffered);
      }
    },
    detachTerminal(terminalId) {
      sinks.delete(terminalId);
    },
    sendInput(terminalId, data) {
      const bytes = new TextEncoder().encode(data);
      seq += 1;
      const item: QueuedInput = { seq, terminalId, data: bytes };
      /** Queued first, sent second: a keystroke that races a closing socket is
       * then still in the queue for the next connection rather than lost. */
      held.push(item);
      send({
        type: "input",
        seq: item.seq,
        terminal_id: terminalId,
        data: encodeBase64(bytes),
      });
    },
    sendCommand,
    close() {
      closedForGood = true;
      if (retryTimer !== null) {
        clearTimeout(retryTimer);
        retryTimer = null;
      }
      socket?.close();
      socket = null;
    },
  };
}
