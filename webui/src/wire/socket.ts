/**
 * The live session: the web protocol over `GET /ws` (D12).
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

import { isStale } from "../state/connection";
import type { ShutdownReason } from "../state/model";
import type { Store } from "../ui/store";
import {
  agoLabel,
  configDocOf,
  dialogOf,
  gitStatusOf,
  seatOf,
  snapshotFromWire,
  statusFromLabel,
} from "./adapt";
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
  type WireConfiguration,
  type WireGitStatus,
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

/**
 * How often 2d's frozen clock re-reads its own age while the picture is a
 * photograph. One second, because the label it feeds counts in seconds
 * (`terminal stale 34s`) and a slower tick would show a number the user can
 * watch being wrong.
 */
const STALE_TICK_MS = 1_000;

/**
 * How long Q3's *"output older than the host's buffer was lost"* stays on
 * screen **after** the replay has landed.
 *
 * The host answers a resume with **one** `TermBytes` per terminal
 * (`stream.rs`'s `attach_frames` → `resume_frame`), so the catching-up state
 * itself can be over in a few milliseconds — far too short to read a sentence
 * that is the whole reason `truncated` is on the wire. The notice therefore
 * outlives the state that produced it. It is not a claim that anything is
 * still replaying: by then `bytesDone === bytesTotal`, the connection reads
 * `connected` again, and the pane prints the loss in the past tense.
 */
const TRUNCATION_NOTICE_MS = 8_000;

/**
 * The catching-up state's dead-man's handle.
 *
 * `catching_up` is entered from the *snapshot* (which is where the outstanding
 * byte count is known) and left when those bytes arrive. A host that promised
 * a backlog in `TerminalView::byte_len` and then never sent it would otherwise
 * leave the tab spinning for ever, which is a worse bug than the one this
 * state exists to fix.
 */
const CATCH_UP_TIMEOUT_MS = 10_000;

/**
 * `34s` / `4m` / `2h` — a bare duration, not `agoLabel`'s "N ago".
 *
 * 2c prints it as `terminal stale 34s` and 2d as `frozen 34s ago`, so the word
 * belongs to the sentence and not to the number. Under a second it reads `0s`
 * rather than borrowing `agoLabel`'s `just now`: the chip's whole job is to be
 * a counter the user can watch climb, and a phrase that has to become a number
 * a moment later reads as a glitch.
 */
export function staleLabel(millis: number): string {
  const seconds = Math.max(0, Math.round(millis / 1000));
  if (seconds < 60) {
    return `${seconds}s`;
  }
  if (seconds < 3600) {
    return `${Math.round(seconds / 60)}m`;
  }
  return `${Math.round(seconds / 3600)}h`;
}

/**
 * A resume being drained, from the snapshot that announced it to the frame
 * that finishes it.
 *
 * Every number here is the host's: `total` is `TerminalView::byte_len` minus
 * the offset the host will actually resume from, and `done` counts the bytes
 * of the frames that really arrived. Nothing is estimated, which is the whole
 * difference between this and a progress bar that reads 100% because both ends
 * were set to the same value.
 */
interface CatchUp {
  readonly terminalId: string;
  readonly fromByte: number;
  readonly total: number;
  done: number;
  truncated: boolean;
}

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
  /**
   * Re-`Attach` asking for a different seat (D14, 2f).
   *
   * Takeover has no dedicated frame — `take_over` is an `Attach`, and it both
   * seats this browser as a writer and takes the input lock from whoever holds
   * it. `observe` gives up contending altogether; `write` asks only for the
   * role, which is never refused.
   *
   * The host answers every one of them with a `Snapshot` and a `Delta::Seats`,
   * so nothing here has to guess what it got.
   */
  requestSeat(seat: "write" | "take_over" | "observe"): void;
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
  /**
   * When this tab's most recent keystroke was **applied** — 2f's "the last one
   * that landed was 3s ago".
   *
   * Deliberately the ack and not the send: a keystroke that was queued, or
   * refused with `seat_held`, never landed, and dating the panel from it would
   * tell the reader their typing was arriving when it was not. `null` until one
   * does, and the panel then leaves the clause out rather than inventing a time.
   *
   * Both ends of the subtraction are this machine's clock, which is what makes
   * it honest without a host timestamp: it measures the gap between two local
   * events, never a host instant against a local clock.
   */
  let lastAppliedInputAtMs: number | null = null;
  /**
   * When the last `term_bytes` frame arrived — the instant 2d's frozen clock
   * names ("the time of the last byte that arrived"), and the instant its
   * `frozen 34s ago` counts from.
   *
   * A **local** instant on purpose, and not a host one. The host does not
   * timestamp `TermBytes` (it is the hot path; a clock on every frame would be
   * a per-frame cost for a fact only a stopped stream ever needs), and it does
   * not have to: staleness is a statement about *this browser's* stream. Both
   * ends of the subtraction below are therefore this machine's clock, the same
   * property that makes `lastAppliedInputAtMs` honest — never a host instant
   * dated against `Date.now()`.
   */
  let lastBytesAtMs: number | null = null;
  /** The photograph's own moment, fixed the instant the picture froze. */
  let frozenAtMs: number | null = null;
  let staleTimer: ReturnType<typeof setInterval> | null = null;
  let staleLabelShown: string | null = null;
  let unsubscribe: (() => void) | null = null;
  /** The resume being drained, or `null` when the stream is continuous. */
  let catchUp: CatchUp | null = null;
  let catchUpTimer: ReturnType<typeof setTimeout> | null = null;
  let noticeTimer: ReturnType<typeof setTimeout> | null = null;
  /**
   * When the outstanding half of a request/response pair was sent, so its
   * answer can be dated — the latency readout, and 2c's `● connected 18ms`.
   *
   * **A round trip measured end to end on one clock.** The host sends
   * `Snapshot::server_time_ms`, and subtracting it from `Date.now()` would be
   * the obvious way to get a number here — and would be wrong by however far
   * the two machines' clocks have drifted, silently, with no way to tell a
   * 200ms link from a 200ms clock offset. The two pairs below are answers the
   * host sends *immediately* on receipt (`Attach` → `Snapshot`, and
   * `Command` → `Ack`), so the gap between sending and reading them is the
   * link, measured twice on the same clock.
   */
  let attachSentAtMs: number | null = null;
  const commandSentAtMs = new Map<number, number>();
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

  // -- 2d's frozen clock (§5.1's stale terminal) ---------------------------

  /**
   * Keep `staleness` in step with the store's own answer to "is the picture on
   * screen a photograph".
   *
   * The predicate is `state/connection.ts`'s `isStale` — the same function
   * `paneTone` uses to decide the amber cast — rather than a second list of
   * connection states kept in this file. That matters because two of the four
   * ways to become stale (an access screen, an eviction) never reach the
   * transport as a connection change: a copy of the rule here would have drawn
   * a photograph with no clock on it, in exactly the two states 2b and 2f
   * spell the fact out in words.
   *
   * This is the only layer allowed to do it. The store and the view may not
   * read a clock (`ui/tokens.guard.test.ts` rule 3), and a duration that is
   * *always* growing has to be re-read from somewhere; the transport owns
   * `now`, so the transport owns the tick.
   */
  function syncStaleness(): void {
    const stale = isStale(store.getState());
    if (!stale) {
      if (frozenAtMs === null) {
        return;
      }
      frozenAtMs = null;
      staleLabelShown = null;
      if (staleTimer !== null) {
        clearInterval(staleTimer);
        staleTimer = null;
      }
      store.dispatch({ type: "staleness/set", staleness: null });
      return;
    }
    if (frozenAtMs !== null) {
      return;
    }
    /**
     * The picture froze at the last byte that arrived. With no bytes yet there
     * is no earlier honest instant than this one, and "0s" is then true rather
     * than flattering: nothing has been shown to go out of date.
     */
    frozenAtMs = lastBytesAtMs ?? now();
    /** Set before the dispatch below, so the re-entrant `syncStaleness` the
     * dispatch triggers finds the work already done and stops. */
    publishStaleness();
    if (staleTimer === null) {
      staleTimer = setInterval(publishStaleness, STALE_TICK_MS);
    }
  }

  function publishStaleness(): void {
    if (frozenAtMs === null) {
      return;
    }
    const label = staleLabel(now() - frozenAtMs);
    if (label === staleLabelShown) {
      return;
    }
    staleLabelShown = label;
    store.dispatch({
      type: "staleness/set",
      staleness: {
        /**
         * A wall-clock time, because 2d chose one: `16:41:08` is checkable
         * against the user's own memory of when they last looked, and a
         * duration is not. It is this browser's wall clock — the same one
         * `onShutdown` stamps its `atLabel` from — because the reader is here,
         * not on the host.
         */
        frozenAt: new Date(frozenAtMs).toLocaleTimeString(),
        ago: label,
      },
    });
  }

  // -- Q3's catching-up state ---------------------------------------------

  /** Stop drawing the replay, whatever phase it was in. */
  function clearCatchUp(): void {
    catchUp = null;
    if (catchUpTimer !== null) {
      clearTimeout(catchUpTimer);
      catchUpTimer = null;
    }
    if (noticeTimer !== null) {
      clearTimeout(noticeTimer);
      noticeTimer = null;
    }
  }

  function publishReplay(state: CatchUp): void {
    store.dispatch({
      type: "replay/set",
      replay: {
        bytesDone: state.done,
        bytesTotal: state.total,
        fromByte: state.fromByte,
        truncated: state.truncated,
      },
    });
  }

  /**
   * The replay landed (or gave up waiting): the stream is continuous again.
   *
   * Q3's warning is the one thing that outlives the state — see
   * `TRUNCATION_NOTICE_MS`. The bytes are already on screen by then, so the
   * pane prints the loss and nothing else; `connection` is `connected`, so
   * neither the strip nor the pane claims a replay is still running.
   */
  function finishCatchUp(): void {
    const finished = catchUp;
    clearCatchUp();
    /**
     * Leave only the state we are actually in. A `Shutdown` can land while a
     * replay is still draining, and Q5 is emphatic that a host which said
     * goodbye must never be described as coming back — a blind `connected`
     * here would clear the terminal state the goodbye had just set.
     */
    if (store.getState().connection === "catching_up") {
      store.dispatch({ type: "connection/changed", status: "connected" });
    }
    if (finished === null || !finished.truncated) {
      store.dispatch({ type: "replay/set", replay: null });
      return;
    }
    noticeTimer = setTimeout(() => {
      noticeTimer = null;
      store.dispatch({ type: "replay/set", replay: null });
    }, TRUNCATION_NOTICE_MS);
  }

  /**
   * Decide, from the snapshot alone, whether this attach is a resume with a
   * backlog — and how big it is.
   *
   * Three host facts and one local one, no estimates:
   *
   *   - `cursors` — the offset this tab last consumed, which is exactly what
   *     `Attach::cursors` just asked to resume from. **Absent means a first
   *     attach**, and a first attach is not catching up on anything: it has
   *     missed nothing, so the whole ring is simply its history.
   *   - `TerminalView::replay_from` — the oldest byte the host still holds. The
   *     host resumes from `max(cursor, replay_from)`, and the inequality
   *     `replay_from > cursor` is *the* definition of `TermBytes::truncated`
   *     (`stream.rs`'s `resume_frame`), so the browser can predict the flag
   *     rather than wait for it. The frame is still believed over this when it
   *     arrives.
   *   - `TerminalView::byte_len` — the end of the stream, which makes the
   *     outstanding count a subtraction rather than a guess.
   *
   * Returns `null` when there is nothing to drain, which covers both "you are
   * up to date" and "the ring is empty" (`replay_from === byte_len`) — the two
   * cases where `resume_frame` sends no frame at all and a catching-up state
   * would hang waiting for one.
   */
  function catchUpFromSnapshot(frame: WireSnapshot): CatchUp | null {
    const terminalId = store.getState().selection?.terminalId ?? null;
    if (terminalId === null) {
      return null;
    }
    const cursor = cursors.get(terminalId);
    if (cursor === undefined) {
      return null;
    }
    const view = frame.projects
      .flatMap((project) => project.sessions)
      .flatMap((session) => session.terminals)
      .find((terminal) => terminal.terminal_id === terminalId);
    if (view === undefined) {
      return null;
    }
    const fromByte = Math.max(cursor, view.replay_from);
    const total = view.byte_len - fromByte;
    if (total <= 0) {
      return null;
    }
    return {
      terminalId,
      fromByte,
      total,
      done: 0,
      truncated: view.replay_from > cursor,
    };
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
    /**
     * One half of the latency readout: the host answers `Attach` with this
     * frame and nothing else in between, so the gap is the round trip. Cleared
     * either way, because a `Snapshot` that came from `request_snapshot`
     * rather than an attach is dated by that command's own ack instead.
     */
    if (attachSentAtMs !== null) {
      store.dispatch({
        type: "latency/set",
        latencyMs: Math.max(0, Math.round(now() - attachSentAtMs)),
      });
      attachSentAtMs = null;
    }
    /** Everything the host has already applied leaves the held queue (§5.1). */
    held = held.filter((item) => item.seq > frame.last_input_seq);
    seq = Math.max(seq, frame.last_input_seq);
    store.dispatch({ type: "snapshot/received", snapshot: snapshotFromWire(frame) });
    store.dispatch({ type: "connection/changed", status: "connected" });
    store.dispatch({ type: "retry/set", retry: null });
    /** 1a is drawn in Terminal mode, and a browser that just attached is here
     * to watch a terminal. */
    store.dispatch({ type: "mode/set", mode: "terminal" });
    /**
     * Q3's catching-up state, entered here because **this is the frame that
     * knows how much is outstanding**: the host announces the end of the
     * stream in `TerminalView::byte_len` and then sends the backlog, so the
     * total is available before the first byte of it is. Reading it off the
     * arriving frames instead is what produced a bar that could only ever say
     * 100%.
     *
     * A previous notice is dropped first: a new attach has a new answer, and a
     * warning about the *last* resume must not be re-dated onto this one.
     */
    clearCatchUp();
    const resume = catchUpFromSnapshot(frame);
    if (resume !== null) {
      catchUp = resume;
      publishReplay(resume);
      store.dispatch({ type: "connection/changed", status: "catching_up" });
      catchUpTimer = setTimeout(finishCatchUp, CATCH_UP_TIMEOUT_MS);
    } else {
      store.dispatch({ type: "replay/set", replay: null });
    }
    flushHeld();
  }

  function onTermBytes(frame: WireTermBytes): void {
    const bytes = decodeBase64(frame.data);
    cursors.set(frame.terminal_id, frame.offset + bytes.length);
    lastBytesAtMs = now();
    if (catchUp !== null && frame.terminal_id === catchUp.terminalId) {
      catchUp.done += bytes.length;
      /** The host's own word wins over the prediction made from the snapshot;
       * they agree by construction, and if they ever stop agreeing the frame
       * is the one that was actually sent. */
      catchUp.truncated = catchUp.truncated || frame.truncated === true;
      publishReplay(catchUp);
      if (catchUp.done >= catchUp.total) {
        finishCatchUp();
      }
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
        /** The same mapping the snapshot path uses, deliberately: 2f draws
         * the same three facts however the seat news arrived. */
        const rows = seats.map((s) => seatOf(s, serverTimeMs));
        store.dispatch({
          type: "seats/changed",
          seat: (frame.you as "writing" | "observing") ?? "observing",
          seats: rows,
        });
        /**
         * 2f's *evicted* panel, and the one condition it may open on.
         *
         * **Not "the lock left me".** Under D14 as revised the lock leaves a
         * writer every time the other person starts a sentence and comes back
         * `INPUT_LOCK_IDLE_MS` after they stop; opening a modal on that would
         * put a dialog in front of somebody several times a minute for an event
         * they were about to stop noticing. The panel is for the one movement a
         * human *confirmed*, and the host is the only place that knows which
         * one that was — hence the per-recipient
         * `Delta::Seats::you_were_preempted` rather than a comparison against
         * the previous rows.
         *
         * A host that never sends the field never opens the panel, which is
         * where this browser already was: `evicted` was modelled, styled and
         * tested from turn 2 and had no dispatcher at all until the host could
         * say *deliberately*.
         */
        if (frame.you_were_preempted === true) {
          const holder = rows.find((row) => row.holdsInput) ?? null;
          store.dispatch({
            type: "takeover/evicted",
            /** The same three-way fallback the `seat_held` path uses: the
             * host-observed address, else the merged label (which starts with
             * it), else a phrase that claims nothing. The label is never split
             * to recover the address — see `WireSeatInfo`. */
            byAddress: holder?.address ?? holder?.label ?? "another writer",
            lastInputAgo:
              lastAppliedInputAtMs === null
                ? ""
                : agoLabel(now() - lastAppliedInputAtMs),
          });
        }
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
      /**
       * D14 as revised: another writer is mid-burst, so the keystroke this
       * refers to was refused rather than mixed into theirs. 2f's panel opens
       * naming them.
       *
       * **We do not re-attach as an observer.** v1 did, because `seat_held`
       * then meant the seat itself was taken and sitting on a socket with no
       * seat was the alternative. It now means only that the turn is somebody
       * else's, for as long as they keep typing — dropping to read-only would
       * give up a seat the host never took, and would leave the tab silently
       * unable to type after the other person stopped.
       */
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
          address: incumbent?.address ?? incumbent?.label ?? "another writer",
          browser: incumbent?.user_agent_label ?? "",
          connected: "",
        },
      });
      return;
    }
    console.warn(`[ws] ${frame.code}: ${frame.message}`);
  }

  function onAck(frame: WireAck): void {
    /**
     * The other half of the latency readout, and the one that keeps it fresh:
     * every command the host settles itself is answered on receipt, and
     * `requestSnapshotSoon` sends one whenever a delta arrives that the store
     * only takes wholesale — so an ordinary session re-measures the link
     * several times a minute without a heartbeat frame of its own.
     *
     * Input acks are deliberately *not* used. They travel through the input
     * lock and the PTY write, so a `seat_held` refusal or a busy terminal
     * would be reported to the user as network latency, which is a different
     * fact wearing the same number.
     */
    const sentAt = commandSentAtMs.get(frame.seq);
    if (sentAt !== undefined) {
      commandSentAtMs.delete(frame.seq);
      store.dispatch({
        type: "latency/set",
        latencyMs: Math.max(0, Math.round(now() - sentAt)),
      });
    }
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
      lastAppliedInputAtMs = now();
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

  /**
   * SPECS §21's panel, in answer to this tab's `show_git_status`
   * (`remote-control-ll5.8`, `specs/WEB_INTERFACE.md` §6.5 R16).
   *
   * Per-viewer, so it only ever arrives because *this* tab asked — the host
   * does not broadcast one reader's read. It comes beside the `ack` for the
   * same seq rather than instead of it: `onAck` above settles the palette row
   * ("applied"), and this opens the panel with what the command produced.
   *
   * Nothing is filtered on the seq here. The frame is addressed to this
   * viewer and carries the seq for the reader's benefit; dropping a panel
   * because a seq did not match a queue this function does not own would lose
   * an answer the host really sent.
   */
  function onGitStatus(frame: WireGitStatus): void {
    store.dispatch({
      type: "gitStatus/received",
      panel: gitStatusOf(frame),
    });
  }

  /**
   * SPECS §8's configuration manager, in answer to this tab's
   * `open_configuration` (`remote-control-1p22`, §6.5 R22).
   *
   * Per-viewer like `git_status`, and handled the same way: the ack settles the
   * palette row, this opens the panel with what the command produced. A save
   * arrives as one of these too — the host applied the edits and re-resolved
   * the layering, and this frame is that answer, so the panel never repaints
   * from the browser's own optimism.
   */
  function onConfiguration(frame: WireConfiguration): void {
    store.dispatch({ type: "config/received", doc: configDocOf(frame) });
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
      case "git_status":
        onGitStatus(frame as WireGitStatus);
        return;
      case "configuration":
        onConfiguration(frame as WireConfiguration);
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

  function sendAttach(seat: "write" | "take_over" | "observe"): void {
    if (socket === null || socket.readyState !== WebSocket.OPEN) {
      return;
    }
    /** `attached` gates every *other* frame, so it has to be true for the
     * attach itself to go out. */
    attached = true;
    /** The clock starts on the request; `onSnapshot` reads it off the answer. */
    attachSentAtMs = now();
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
      sendAttach("write");
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
      /**
       * A replay that was still draining when the link died is not draining
       * any more, and its dead-man's handle must not fire a `connected` into a
       * tab that is reconnecting. The next snapshot recomputes the backlog
       * from scratch, which is the only honest source for it.
       */
      clearCatchUp();
      attachSentAtMs = null;
      commandSentAtMs.clear();
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
    const went = send(args === undefined
      ? { type: "command", seq: mySeq, name }
      : { type: "command", seq: mySeq, name, args });
    /** Only a frame that actually left can be timed; one refused by a closed
     * socket would date its eventual answer from long before it was asked. */
    if (went) {
      commandSentAtMs.set(mySeq, now());
    }
    return mySeq;
  }

  /**
   * 2d's frozen clock follows the *store's* idea of staleness, not the
   * socket's, so it is driven from a subscription rather than from the
   * connection callbacks — see `syncStaleness`.
   */
  unsubscribe = store.subscribe(syncStaleness);

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
    requestSeat(seat) {
      sendAttach(seat);
    },
    close() {
      closedForGood = true;
      if (retryTimer !== null) {
        clearTimeout(retryTimer);
        retryTimer = null;
      }
      if (staleTimer !== null) {
        clearInterval(staleTimer);
        staleTimer = null;
      }
      clearCatchUp();
      unsubscribe?.();
      unsubscribe = null;
      socket?.close();
      socket = null;
    },
  };
}
