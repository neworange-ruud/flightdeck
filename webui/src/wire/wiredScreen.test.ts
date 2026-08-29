/**
 * @vitest-environment jsdom
 *
 * `remote-control-74q0` — the three states that were rendered, styled and unit
 * tested, and never dispatched.
 *
 * **Every test in this file drives a frame, never an action.** That is the
 * whole point of the file existing beside `socket.test.ts`'s plain-node cases:
 * the bug it was written for is one no unit test could have caught, because a
 * test that dispatches `staleness/set` (or `replay/set`, or `latency/set`)
 * supplies the exact thing production was missing and then asserts that the
 * renderer — which was never broken — draws it. So the store here is the *real*
 * app's store, `openSession` is wired to it through a fake `WebSocket`, and
 * every assertion is made against the DOM the user would be looking at.
 *
 * The three, and where each number comes from:
 *
 *   1. **Catching up (Q3).** `TerminalView::byte_len` minus the cursor this tab
 *      resumed from is how much is outstanding; the frames that arrive are how
 *      much has landed. Both are the host's, neither is invented.
 *   2. **Staleness (2c/2d).** The gap between two *local* events — the last
 *      byte in, and now — which is the only pair a browser can measure without
 *      a host clock to be wrong about.
 *   3. **Latency (2c).** One round trip, `Attach` → `Snapshot`, timed on one
 *      clock at one end.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createApp } from "../ui/app";
import type { App } from "../ui/app";
import { openSession } from "./socket";
import { encodeBase64, PROTOCOL_VERSION } from "./frames";

const TERMINAL = "tab-1:primary";

interface FakeSocket {
  onopen: (() => void) | null;
  onmessage: ((event: { data: string }) => void) | null;
  onclose: (() => void) | null;
  onerror: (() => void) | null;
  readonly readyState: number;
  send(data: string): void;
  close(): void;
}

function fakeSocket(): FakeSocket {
  return {
    onopen: null,
    onmessage: null,
    onclose: null,
    onerror: null,
    readyState: 1,
    send: () => undefined,
    close: () => undefined,
  };
}

/** One host snapshot. `byteLen`/`replayFrom` are the two facts Q3's resume
 * arithmetic is made of; `git` is here for the one test that is about what git
 * said rather than about bytes, and defaults to 1a's own numbers. */
function snapshotFrame(over: {
  readonly byteLen: number;
  readonly replayFrom: number;
  readonly git?: Record<string, unknown>;
}): string {
  return JSON.stringify({
    type: "snapshot",
    protocol_version: PROTOCOL_VERSION,
    host_version: "1.16.0",
    server_time_ms: 1_000_000,
    viewer_id: "v1",
    seat: "writing",
    seats: [
      {
        viewer_id: "v1",
        label: "127.0.0.1 · Chrome",
        seat: "writing",
        holds_input: true,
        since_ms: 999_000,
        is_you: true,
      },
    ],
    last_input_seq: 0,
    projects: [
      {
        project_id: "/repo",
        name: "flightdeck",
        root: "/repo",
        base_branch: "main",
        sessions: [
          {
            session_id: "tab-1",
            project_id: "/repo",
            name: "fix-login",
            agent: "claude",
            agent_display_name: "Claude Code",
            phase: "ready",
            status: {
              interpreted: "working",
              manual: null,
              bucket: "in_progress",
              running_time_secs: 12,
            },
            git: over.git ?? {
              branch: "flightdeck/fix-login",
              added: 3,
              modified: 2,
              removed: 1,
              ahead: 3,
              behind: 0,
              drift: 4,
              has_upstream: true,
              files_changed: 6,
              collected: true,
            },
            terminals: [
              {
                terminal_id: TERMINAL,
                session_id: "tab-1",
                role: "primary",
                title: "agent",
                geometry: { cols: 120, rows: 34 },
                byte_len: over.byteLen,
                replay_from: over.replayFrom,
                alive: true,
              },
            ],
            lifecycle_reporting: true,
          },
        ],
      },
    ],
    selection: {
      project_id: "/repo",
      session_id: "tab-1",
      terminal_id: TERMINAL,
    },
    geometry: { cols: 120, rows: 34 },
    replay_capacity_bytes: 262_144,
    activity: [],
  });
}

function termBytesFrame(offset: number, length: number): string {
  return JSON.stringify({
    type: "term_bytes",
    terminal_id: TERMINAL,
    offset,
    data: encodeBase64(new Uint8Array(length).fill(0x2e)),
  });
}

interface Harness {
  readonly app: App;
  readonly sockets: FakeSocket[];
  /** The live socket — a reconnect makes a new one. */
  live(): FakeSocket;
  deliver(frame: string): void;
  /** Advance the *injected* clock, which is not the timer clock. */
  elapse(ms: number): void;
  text(selector: string): string;
  el(selector: string): HTMLElement;
}

/**
 * Live sessions, closed between tests.
 *
 * Not hygiene for its own sake: a stale terminal ticks its own clock, so a
 * session left open would keep dispatching into a store nothing is looking at
 * any more — for the rest of the run, on real timers, once this file's fake
 * ones are restored.
 */
const open: { close(): void }[] = [];

function harness(): Harness {
  let clock = 1_700_000_000_000;
  const sockets: FakeSocket[] = [];
  const app = createApp({ mount: () => undefined, now: () => clock });
  document.body.append(app.el);
  open.push(openSession({
    store: app.store,
    url: "ws://test/ws",
    socketFactory: () => {
      const ws = fakeSocket();
      sockets.push(ws);
      return ws as unknown as WebSocket;
    },
    now: () => clock,
  }));
  const live = (): FakeSocket => {
    const ws = sockets[sockets.length - 1];
    if (ws === undefined) {
      throw new Error("no socket was opened");
    }
    return ws;
  };
  /** The host will not answer a frame that was never attached, and neither
   * will the socket send one — so every harness starts the handshake. */
  live().onopen?.();
  return {
    app,
    sockets,
    live,
    deliver: (frame) => live().onmessage?.({ data: frame }),
    elapse: (ms) => {
      clock += ms;
    },
    text: (selector) =>
      document.querySelector<HTMLElement>(selector)?.textContent ?? "",
    el: (selector) => {
      const found = document.querySelector<HTMLElement>(selector);
      if (found === null) {
        throw new Error(`no element matched ${selector}`);
      }
      return found;
    },
  };
}

beforeEach(() => {
  vi.useFakeTimers();
  document.body.replaceChildren();
});

afterEach(() => {
  for (const session of open.splice(0)) {
    session.close();
  }
  vi.useRealTimers();
});

/**
 * Drop the link and come back on a new socket, exactly as the retry loop does
 * it — the *only* path in the app that produces a resume, and therefore the
 * only honest way to reach the catching-up state.
 */
function reconnect(h: Harness): void {
  h.live().onclose?.();
  /** `RETRY_DELAYS_MS[0]` is 250ms; anything past it opens the next socket. */
  vi.advanceTimersByTime(300);
  h.live().onopen?.();
}

describe("Q3's catching-up state, reached the way production reaches it", () => {
  it("enters catching_up on a resume with a backlog, and leaves it when the replay lands", () => {
    const h = harness();
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));
    /** A first attach has missed nothing: there is no cursor to resume from,
     * so the whole ring is simply this tab's history. */
    expect(h.app.store.getState().connection).toBe("connected");
    expect(h.app.store.getState().replay).toBeNull();

    h.deliver(termBytesFrame(0, 8));
    reconnect(h);
    expect(h.app.store.getState().connection).toBe("reconnecting");

    /** 41 bytes were written while this tab was away: `byte_len` 49 against a
     * cursor of 8. Neither number is this browser's. */
    h.deliver(snapshotFrame({ byteLen: 49, replayFrom: 0 }));
    expect(h.app.store.getState().connection).toBe("catching_up");
    expect(h.app.store.getState().replay).toEqual({
      bytesDone: 0,
      bytesTotal: 41,
      fromByte: 8,
      truncated: false,
    });
    /** 2c's row and 2d's pane, both reached without dispatching anything. */
    expect(h.text(".fd-statusbar")).toContain("catching up");
    expect(h.text(".fd-statusbar")).toContain("input queues until the replay lands");
    expect(h.el(".fd-pane").getAttribute("data-tone")).toBe("catching_up");
    expect(h.text(".fd-pane__banner")).toContain("replaying 41 B…");

    h.deliver(termBytesFrame(8, 41));
    expect(h.app.store.getState().connection).toBe("connected");
    expect(h.app.store.getState().replay).toBeNull();
    expect(h.el(".fd-pane").getAttribute("data-tone")).toBe("live");
  });

  it("draws a progress bar out of the host's numbers, not out of the frame it just got", () => {
    /**
     * The regression this is really about: `bytesDone` and `bytesTotal` were
     * both set to the length of the arriving frame, so the bar read 100% for
     * every replay ever. The total now comes from the snapshot — *before* a
     * byte of it has arrived — so a half-drained replay reads half.
     */
    const h = harness();
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));
    h.deliver(termBytesFrame(0, 8));
    reconnect(h);
    h.deliver(snapshotFrame({ byteLen: 108, replayFrom: 0 }));

    const bar = h.el("progress.fd-replay");
    expect(bar.getAttribute("value")).toBe("0");
    expect(bar.getAttribute("max")).toBe("100");

    /** A host free to chunk its answer, which the browser must not assume it
     * will not do. */
    h.deliver(termBytesFrame(8, 40));
    expect(h.app.store.getState().connection).toBe("catching_up");
    expect(h.el("progress.fd-replay").getAttribute("value")).toBe("40");
    expect(h.text(".fd-pane__banner")).toContain("replaying 60 B…");

    h.deliver(termBytesFrame(48, 60));
    expect(h.app.store.getState().connection).toBe("connected");
  });

  it("says Q3's sentence when the ring aged out, and keeps saying it after the replay lands", () => {
    /**
     * The serious half of the bug. `truncated` exists to buy one sentence, and
     * that sentence could not be reached at all: `replay/set` only ever fired
     * on a truncated frame, and the pane only draws replay children while the
     * tone is `catching_up` — a tone nothing entered.
     *
     * It is asserted *twice* on purpose. The host answers a resume with one
     * frame per terminal, so catching-up can be over in a millisecond; a
     * warning that is only legible during it is a warning nobody reads.
     */
    const h = harness();
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));
    h.deliver(termBytesFrame(0, 8));
    reconnect(h);

    /** The ring now starts at 20, past this tab's cursor of 8: bytes 8–19 are
     * gone. That inequality is exactly how the host decides `truncated`. */
    h.deliver(snapshotFrame({ byteLen: 49, replayFrom: 20 }));
    expect(h.app.store.getState().replay).toEqual({
      bytesDone: 0,
      bytesTotal: 29,
      fromByte: 20,
      truncated: true,
    });
    expect(h.text(".fd-pane__banner")).toContain(
      "output older than the host's buffer was lost — this is not a continuous replay",
    );

    h.deliver(termBytesFrame(20, 29));
    expect(h.app.store.getState().connection).toBe("connected");
    /** Still on screen, now in the past tense, over a terminal that is live
     * again. Nothing here claims a replay is still running. */
    expect(h.el(".fd-pane__banner").hidden).toBe(false);
    expect(h.text(".fd-pane__banner")).toContain(
      "output older than the host's buffer was lost",
    );
    expect(h.text(".fd-pane__banner")).not.toContain("replaying");

    vi.advanceTimersByTime(9_000);
    expect(h.app.store.getState().replay).toBeNull();
    expect(h.el(".fd-pane__banner").hidden).toBe(true);
  });

  it("does not enter catching_up when the host has nothing to replay", () => {
    /** `Resume::UpToDate` sends no frame at all, so a state entered on the
     * strength of the snapshot alone would hang waiting for one. */
    const h = harness();
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));
    h.deliver(termBytesFrame(0, 8));
    reconnect(h);
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));
    expect(h.app.store.getState().connection).toBe("connected");
    expect(h.app.store.getState().replay).toBeNull();
  });

  it("does not un-stop a host that said goodbye mid-replay (Q5)", () => {
    const h = harness();
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));
    h.deliver(termBytesFrame(0, 8));
    reconnect(h);
    h.deliver(snapshotFrame({ byteLen: 49, replayFrom: 0 }));
    expect(h.app.store.getState().connection).toBe("catching_up");

    h.deliver(
      JSON.stringify({ type: "shutdown", reason: "host_quit", self_initiated: false }),
    );
    /** The last of the replay still turns up — the socket has not closed yet —
     * and must not be read as the host coming back. */
    h.deliver(termBytesFrame(8, 41));
    expect(h.app.store.getState().connection).toBe("stopped");
    expect(h.app.store.getState().shutdown).not.toBeNull();
  });

  it("does not hang if the promised replay never arrives", () => {
    const h = harness();
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));
    h.deliver(termBytesFrame(0, 8));
    reconnect(h);
    h.deliver(snapshotFrame({ byteLen: 49, replayFrom: 0 }));
    expect(h.app.store.getState().connection).toBe("catching_up");

    vi.advanceTimersByTime(11_000);
    expect(h.app.store.getState().connection).toBe("connected");
    expect(h.app.store.getState().replay).toBeNull();
  });
});

describe("2c/2d's staleness, computed rather than assumed", () => {
  it("freezes a clock at the last byte and counts up from it while the link is down", () => {
    const h = harness();
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));
    h.deliver(termBytesFrame(0, 8));
    /** No staleness on a live link: 2d's rule is that colour means live. */
    expect(h.app.store.getState().staleness).toBeNull();

    h.elapse(34_000);
    h.live().onclose?.();

    /**
     * 34 seconds, both ends of the subtraction on this machine's clock: the
     * byte that arrived and the moment it stopped being current. No host
     * instant is dated against `Date.now()` anywhere in it.
     */
    expect(h.app.store.getState().staleness?.ago).toBe("34s");
    expect(h.text(".fd-statusbar")).toContain("terminal stale 34s");
    expect(h.el(".fd-pane").getAttribute("data-tone")).toBe("stale");
    expect(h.text(".fd-pane__banner")).toContain("frozen 34s ago");
    expect(h.text(".fd-pane__banner")).not.toContain("a moment");

    /** And it climbs on its own, which is the half a one-shot dispatch could
     * never do: the chip is a counter the user watches. */
    h.elapse(6_000);
    vi.advanceTimersByTime(1_000);
    expect(h.app.store.getState().staleness?.ago).toBe("40s");
    expect(h.text(".fd-statusbar")).toContain("terminal stale 40s");
  });

  it("keeps the frozen wall-clock fixed while the age moves", () => {
    /** 2d prints a time, not a duration, because `16:41:08` is checkable
     * against the reader's own memory of when they last looked. */
    const h = harness();
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));
    h.deliver(termBytesFrame(0, 8));
    h.live().onclose?.();
    const frozenAt = h.app.store.getState().staleness?.frozenAt;
    expect(frozenAt).toBeTruthy();
    expect(h.text(".fd-pane__clock")).toBe(frozenAt);

    h.elapse(20_000);
    vi.advanceTimersByTime(1_000);
    expect(h.app.store.getState().staleness?.frozenAt).toBe(frozenAt);
    expect(h.app.store.getState().staleness?.ago).toBe("20s");
  });

  it("clears the photograph the moment the host answers again", () => {
    const h = harness();
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));
    h.deliver(termBytesFrame(0, 8));
    h.live().onclose?.();
    expect(h.app.store.getState().staleness).not.toBeNull();

    reconnect(h);
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));
    expect(h.app.store.getState().staleness).toBeNull();
    expect(h.text(".fd-statusbar")).not.toContain("terminal stale");
  });
});

describe("2c's latency readout, measured rather than declared", () => {
  it("times the attach against its snapshot and prints the number", () => {
    const h = harness();
    /** The link, twice, on one clock: `sendAttach` stamped the request when
     * `onopen` fired, and this is the answer. */
    h.elapse(18);
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));

    expect(h.app.store.getState().latencyMs).toBe(18);
    expect(h.text(".fd-statusbar")).toContain("connected");
    expect(h.text(".fd-statusbar")).toContain("18ms");
  });

  it("re-measures on the acks of the commands the app already sends", () => {
    /**
     * Without this the readout would be a single number taken at attach and
     * never revisited. `request_snapshot` is sent by `requestSnapshotSoon`
     * whenever a delta arrives that the store only takes wholesale, and the
     * host answers it on receipt — so an ordinary session re-measures without
     * a heartbeat frame of its own.
     */
    const h = harness();
    h.elapse(18);
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));
    expect(h.app.store.getState().latencyMs).toBe(18);

    h.deliver(
      JSON.stringify({ type: "delta", change: "status", session_id: "tab-1" }),
    );
    /** The coalescing window, then the command goes out. */
    vi.advanceTimersByTime(200);
    h.elapse(21);
    h.deliver(JSON.stringify({ type: "ack", seq: 1, outcome: "applied" }));
    expect(h.app.store.getState().latencyMs).toBe(21);
    expect(h.text(".fd-statusbar")).toContain("21ms");
  });

  it("renders a bare `connected` until a round trip has actually been measured", () => {
    /** Never a fabricated zero: `null` means nothing has been timed yet, and
     * 2c's second half is simply absent. */
    const h = harness();
    expect(h.app.store.getState().latencyMs).toBeNull();
    expect(h.text(".fd-statusbar")).not.toContain("ms");
  });
});

describe("R2's ahead/behind, which cannot exist without an upstream", () => {
  it("says no-upstream in the bar and the row at once, off the host's own bool", () => {
    /**
     * The defect this is written for: the bar printed `↑0 ↓0` titled "commits
     * ahead of and behind the upstream" while the sidebar row three inches
     * above it said `no-upstream`, on the same screen, from the same frame.
     * Driven as a frame rather than as a `GitBarInfo`, because the field that
     * was missing is one the adapter never read.
     */
    const h = harness();
    h.deliver(
      snapshotFrame({
        byteLen: 8,
        replayFrom: 0,
        git: {
          branch: "flightdeck/fix-login",
          added: 0,
          modified: 0,
          removed: 0,
          /** The zeroes a host sends beside `has_upstream: false`. They are
           * not a measurement, and nothing may print them as one. */
          ahead: 0,
          behind: 0,
          drift: 0,
          has_upstream: false,
          files_changed: 0,
          collected: true,
        },
      }),
    );

    expect(h.text(".fd-gitbar")).toContain("no-upstream");
    expect(h.text(".fd-gitbar")).not.toContain("↑");
    expect(h.text(".fd-gitbar")).not.toContain("↓");
    expect(h.text(".fd-session__facts")).toContain("no-upstream");
    /** 2e's word for a worktree with nothing in it, in place of four zeroes. */
    expect(h.text(".fd-gitbar")).toContain("clean");
    expect(h.text(".fd-gitbar")).not.toContain("(0 files)");
  });

  it("prints the pair when the host says there is an upstream to count against", () => {
    const h = harness();
    h.deliver(snapshotFrame({ byteLen: 8, replayFrom: 0 }));
    expect(h.text(".fd-gitbar")).toContain("↑3 ↓0");
    expect(h.text(".fd-gitbar")).not.toContain("no-upstream");
    expect(h.text(".fd-gitbar")).toContain("+3 ~2 -1 (6 files)");
  });
});
