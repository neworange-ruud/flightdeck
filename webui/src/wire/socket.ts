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
import { snapshotFromWire } from "./adapt";
import {
  decodeBase64,
  encodeBase64,
  PROTOCOL_VERSION,
  WS_PATH,
  type ServerFrame,
  type WireAck,
  type WireDeltaEnvelope,
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
  /** A named command (`protocol::command`). */
  sendCommand(name: string, args?: unknown): void;
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
        store.dispatch({
          type: "seats/changed",
          seat: (frame.you as "controlling" | "observing") ?? "observing",
          seats: seats.map((s) => ({
            label: s.label,
            seat: s.seat,
            isDesktop: s.viewer_id === null,
            sinceLabel: "",
          })),
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
      case "activity": {
        const event = frame as unknown as {
          event_id: string;
          project_id: string;
          project_name: string;
          session_id: string;
          session_name: string;
          reason?: string;
          tier: "attention" | "finished" | "quiet";
          read?: boolean;
        };
        store.dispatch({
          type: "activity/received",
          events: [
            {
              id: event.event_id,
              atLabel: "just now",
              projectId: event.project_id,
              projectName: event.project_name,
              sessionId: event.session_id,
              sessionName: event.session_name,
              from: "unknown",
              to: "unknown",
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
          address: incumbent?.label ?? "another browser",
          browser: incumbent?.label ?? "another browser",
          connected: "",
        },
      });
      sendAttach("observe");
      return;
    }
    console.warn(`[ws] ${frame.code}: ${frame.message}`);
  }

  function onAck(frame: WireAck): void {
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

  function sendCommand(name: string, args?: unknown): void {
    seq += 1;
    send(args === undefined
      ? { type: "command", seq, name }
      : { type: "command", seq, name, args });
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
