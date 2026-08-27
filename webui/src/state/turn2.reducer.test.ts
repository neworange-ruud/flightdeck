import { describe, expect, it } from "vitest";
import { unreadChip, unreadSummary } from "./activity";
import { fixtureActivity, fixtureSnapshot } from "./fixture";
import { reduce } from "./reducer";
import { createInitialState, dropAckedInput, firstPendingSeq } from "./types";
import type { AppState } from "./types";

/**
 * The turn-2 half of the reducer: §5.1's input guarantee, Q5's terminal state,
 * the access keypad, seats and the feed.
 *
 * `src/state/reducer.test.ts` covers the scaffold's actions and is left alone.
 */

function attached(): AppState {
  let state = createInitialState();
  state = reduce(state, {
    type: "snapshot/received",
    snapshot: fixtureSnapshot(),
  });
  return reduce(state, { type: "connection/changed", status: "connected" });
}

function type(state: AppState, keys: readonly string[]): AppState {
  return keys.reduce(
    (next, data) => reduce(next, { type: "input/queue", data }),
    state,
  );
}

describe("§5.1 — input is queued, never dropped, never reordered, never doubled", () => {
  it("queues while reconnecting instead of dropping", () => {
    let state = reduce(attached(), {
      type: "connection/changed",
      status: "reconnecting",
    });
    state = type(state, ["c", "a", "r", "g", "o"]);

    /** There is no branch in the reducer that discards a keystroke because the
     * link is down. That is what the queue is for. */
    expect(state.pendingInput).toEqual(["c", "a", "r", "g", "o"]);
    expect(state.inputSeq).toBe(5);
  });

  it("assigns a monotonic seq at queue time, so order is fixed before any reconnect", () => {
    const state = type(attached(), ["a", "b", "c"]);
    expect(firstPendingSeq(state)).toBe(1);
    expect(state.inputSeq).toBe(3);
  });

  it("a reconnect drops exactly the acknowledged prefix, in order", () => {
    /**
     * The scenario: five keystrokes were queued, the host applied the first two
     * before the socket died, and the reattach's `Snapshot { last_input_seq }`
     * says so. Re-sending all five would double `c`, `a`; re-sending none would
     * lose `g`, `o`.
     */
    let state = type(attached(), ["c", "a", "r", "g", "o"]);
    state = reduce(state, {
      type: "connection/changed",
      status: "reconnecting",
    });
    state = reduce(state, { type: "input/acked", throughSeq: 2 });

    expect(state.pendingInput).toEqual(["r", "g", "o"]);
    /** The counter is never reset, so the next keystroke cannot collide with
     * one the host already has. */
    expect(state.inputSeq).toBe(5);
    expect(firstPendingSeq(state)).toBe(3);
  });

  it("keeps order and count across a full reconnect cycle", () => {
    let state = type(attached(), ["1", "2"]);
    state = reduce(state, { type: "connection/changed", status: "reconnecting" });
    /** More typing against the stale terminal — 2d's case. */
    state = type(state, ["3", "4"]);
    state = reduce(state, { type: "connection/changed", status: "catching_up" });
    state = type(state, ["5"]);
    /** The host had applied `1` before it died. */
    state = reduce(state, { type: "input/acked", throughSeq: 1 });
    state = reduce(state, { type: "connection/changed", status: "connected" });

    expect(state.pendingInput).toEqual(["2", "3", "4", "5"]);
    /** No duplicates: every element appears exactly once, in the order typed. */
    expect(new Set(state.pendingInput).size).toBe(4);
  });

  it("an ack the queue is already past drops nothing", () => {
    let state = type(attached(), ["a"]);
    state = reduce(state, { type: "input/acked", throughSeq: 1 });
    const settled = state;
    /** A redundant ack from the host is not a re-render and not a data loss. */
    state = reduce(state, { type: "input/acked", throughSeq: 1 });
    expect(state).toBe(settled);
    expect(state.pendingInput).toEqual([]);
  });

  it("an ack ahead of the queue drops all of it, never more", () => {
    const state = type(attached(), ["a", "b"]);
    expect(dropAckedInput(state, 99)).toEqual([]);
    expect(dropAckedInput(state, -1)).toEqual(["a", "b"]);
  });

  it("a single Esc is queued like any other keystroke", () => {
    /** §5: `esc to interrupt` is the key users press most, so it passes through
     * — and being in the queue means the ordering guarantee covers it too. */
    const state = reduce(
      { ...attached(), mode: "terminal" },
      { type: "input/esc", at: 1000 },
    );
    expect(state.pendingInput).toEqual(["\x1b"]);
    expect(state.inputSeq).toBe(1);
  });
});

describe("Q5 — a host that said goodbye stays gone", () => {
  it("host_quit lands in a terminal state, not a retry", () => {
    const state = reduce(attached(), {
      type: "connection/shutdown",
      shutdown: {
        reason: "host_quit",
        selfInitiated: false,
        detail: "",
        atLabel: "16:42",
      },
    });
    expect(state.connection).toBe("stopped");
  });

  it("refuses a later reconnecting transition — host-initiated", () => {
    let state = reduce(attached(), {
      type: "connection/shutdown",
      shutdown: {
        reason: "host_quit",
        selfInitiated: false,
        detail: "",
        atLabel: "16:42",
      },
    });
    /**
     * This is the behaviour the requirement is about: a transport that keeps
     * dialling cannot paint "reconnecting" over a host that has exited. The
     * state machine stops the retry loop, not politeness.
     */
    state = reduce(state, { type: "connection/changed", status: "reconnecting" });
    expect(state.connection).toBe("stopped");
    state = reduce(state, { type: "connection/changed", status: "disconnected" });
    expect(state.connection).toBe("stopped");
  });

  it("refuses a later reconnecting transition — self-initiated", () => {
    let state = reduce(attached(), {
      type: "connection/shutdown",
      shutdown: {
        reason: "host_quit",
        selfInitiated: true,
        detail: "6 agents were stopped",
        atLabel: "16:42",
      },
    });
    state = reduce(state, { type: "connection/changed", status: "reconnecting" });
    expect(state.connection).toBe("stopped");
    /** And the flag survives, because it is what chooses the sentence. */
    expect(state.shutdown?.selfInitiated).toBe(true);
  });

  it("a restart is the one reason that does retry", () => {
    const state = reduce(attached(), {
      type: "connection/shutdown",
      shutdown: {
        reason: "restarting",
        selfInitiated: false,
        detail: "",
        atLabel: "16:42",
      },
    });
    expect(state.connection).toBe("reconnecting");
  });

  it("a revoked token goes to the access screens, not to a retry loop", () => {
    let state = reduce(attached(), {
      type: "connection/shutdown",
      shutdown: {
        reason: "token_revoked",
        selfInitiated: false,
        detail: "",
        atLabel: "16:42",
      },
    });
    /** 2b/2c give this different words and a different colour: the host is
     * fine, your credential is not, and the fix is a code. */
    expect(state.connection).toBe("revoked");
    state = reduce(state, { type: "connection/changed", status: "reconnecting" });
    expect(state.connection).toBe("revoked");
  });

  it("lets a host that really came back come back", () => {
    let state = reduce(attached(), {
      type: "connection/shutdown",
      shutdown: {
        reason: "host_quit",
        selfInitiated: false,
        detail: "",
        atLabel: "16:42",
      },
    });
    state = reduce(state, { type: "connection/changed", status: "connected" });
    /** Not a loophole: if it is back, it is back, and leaving a working session
     * behind a dead-end screen would be its own lie. */
    expect(state.connection).toBe("connected");
    expect(state.shutdown).toBeNull();
  });

  it("clears the frozen clock when the picture is live again", () => {
    let state = reduce(attached(), {
      type: "staleness/set",
      staleness: { frozenAt: "16:41:08", ago: "34s" },
    });
    state = reduce(state, { type: "connection/changed", status: "connected" });
    expect(state.staleness).toBeNull();
  });
});

describe("2b — the access keypad", () => {
  function needingCode(): AppState {
    return reduce(attached(), {
      type: "access/required",
      screen: "code_entry",
      attemptsRemaining: null,
      lockoutSeconds: null,
    });
  }

  it("takes four digits and no more", () => {
    let state = needingCode();
    for (const digit of ["8", "4", "1", "2", "9"]) {
      state = reduce(state, { type: "access/digit", digit });
    }
    /** A fifth keystroke means the user mistyped, not that they meant to start
     * over — wrapping would silently discard the code they can see. */
    expect(state.access?.code).toBe("8412");
  });

  it("ignores anything that is not a digit", () => {
    const state = reduce(needingCode(), { type: "access/digit", digit: "a" });
    expect(state.access?.code).toBe("");
  });

  it("backspace takes one digit back and stops at empty", () => {
    let state = reduce(needingCode(), { type: "access/digit", digit: "8" });
    state = reduce(state, { type: "access/backspace" });
    expect(state.access?.code).toBe("");
    expect(reduce(state, { type: "access/backspace" })).toBe(state);
  });

  it("a refusal keeps the digits that failed and clears the box", () => {
    let state = needingCode();
    for (const digit of ["8", "4", "1", "9"]) {
      state = reduce(state, { type: "access/digit", digit });
    }
    state = reduce(state, {
      type: "access/refused",
      screen: "rejected",
      attemptsRemaining: 3,
      lockoutSeconds: null,
    });
    expect(state.access).toMatchObject({
      screen: "rejected",
      refused: "8419",
      code: "",
      attemptsRemaining: 3,
    });
  });

  it("typing again leaves the rejected screen without losing the budget", () => {
    let state = reduce(needingCode(), {
      type: "access/refused",
      screen: "rejected",
      attemptsRemaining: 2,
      lockoutSeconds: null,
    });
    state = reduce(state, { type: "access/digit", digit: "1" });
    /** Continuing to shout "That code did not work" at someone who is answering
     * it is both wrong and in the way. */
    expect(state.access?.screen).toBe("code_entry");
    expect(state.access?.attemptsRemaining).toBe(2);
  });

  it("revocation is a connection state as well as a screen", () => {
    const state = reduce(attached(), {
      type: "access/revoked",
      revokedAgo: "12s ago",
    });
    expect(state.connection).toBe("revoked");
    expect(state.access).toMatchObject({ screen: "revoked", revokedAgo: "12s ago" });
  });

  it("`Stay here` puts the overlay away without claiming to be authorised", () => {
    let state = reduce(attached(), { type: "access/revoked", revokedAgo: "12s ago" });
    state = reduce(state, { type: "access/dismiss" });
    expect(state.access).toBeNull();
    /** The credential did not change, so the strip must still say so. */
    expect(state.connection).toBe("revoked");
  });

  it("granted is the only thing that means authorised", () => {
    let state = reduce(attached(), { type: "access/revoked", revokedAgo: null });
    state = reduce(state, { type: "access/granted" });
    expect(state.access).toBeNull();
  });
});

describe("2f — seats and takeover", () => {
  it("the host's snapshot decides our seat; we never assume it", () => {
    expect(attached().seat).toBe("controlling");
    expect(createInitialState().seat).toBe("observing");
  });

  it("a held seat prompts, and does not pretend we have control", () => {
    const state = reduce(attached(), {
      type: "takeover/held",
      incumbent: {
        address: "192.168.2.20",
        browser: "Safari · iOS 18",
        connected: "14 minutes, active 20s ago",
      },
    });
    expect(state.takeover?.kind).toBe("arriving");
    expect(state.seat).toBe("observing");
  });

  it("eviction leaves the socket open as an observer, never a shutdown", () => {
    const state = reduce(attached(), {
      type: "takeover/evicted",
      byAddress: "192.168.2.11",
      lastInputAgo: "3s",
    });
    expect(state.takeover?.kind).toBe("evicted");
    expect(state.seat).toBe("observing");
    /** A `Delta::Seats`, not a `Shutdown`: the connection is untouched. */
    expect(state.connection).toBe("connected");
    expect(state.shutdown).toBeNull();
  });

  it("claiming clears the prompt and takes the seat", () => {
    let state = reduce(attached(), {
      type: "takeover/evicted",
      byAddress: "192.168.2.11",
      lastInputAgo: "3s",
    });
    state = reduce(state, { type: "takeover/claim" });
    expect(state.takeover).toBeNull();
    expect(state.seat).toBe("controlling");
  });

  it("watching read-only is a destination, from both directions", () => {
    for (const prompt of [
      {
        type: "takeover/held" as const,
        incumbent: { address: "a", browser: "b", connected: "c" },
      },
      {
        type: "takeover/evicted" as const,
        byAddress: "192.168.2.11",
        lastInputAgo: "3s",
      },
    ]) {
      let state = reduce(attached(), prompt);
      state = reduce(state, { type: "takeover/observe" });
      expect(state.takeover).toBeNull();
      expect(state.seat).toBe("observing");
      expect(state.connection).toBe("connected");
    }
  });
});

describe("2e — the feed's state", () => {
  it("backfills from the snapshot rather than appending to it", () => {
    let state = attached();
    expect(state.activity).toHaveLength(fixtureActivity().length);
    /** A reattach re-sends the host's whole store, so appending would double
     * every row the tab had already seen. */
    state = reduce(state, {
      type: "snapshot/received",
      snapshot: fixtureSnapshot(),
    });
    expect(state.activity).toHaveLength(fixtureActivity().length);
  });

  it("appends a live delta", () => {
    const state = reduce(attached(), {
      type: "activity/received",
      events: [
        {
          id: "e-live",
          atLabel: "now",
          projectId: "p-web",
          projectName: "web",
          sessionId: "s-bump-deps",
          sessionName: "bump-deps",
          from: "in_progress",
          to: "waiting",
          reason: "asked a question",
          tier: "attention",
          read: false,
        },
      ],
    });
    expect(state.activity).toHaveLength(fixtureActivity().length + 1);
    expect(unreadSummary(state.activity)?.countAtTier).toBe(3);
  });

  it("read-marking drains the chip to the all-read rendering", () => {
    let state = attached();
    expect(unreadChip(state.activity).tone).toBe("attention");
    state = reduce(state, {
      type: "activity/read",
      ids: state.activity.map((event) => event.id),
    });
    expect(unreadChip(state.activity).tone).toBe("read");
  });

  it("read-marking an already-read id is a no-op", () => {
    const state = attached();
    expect(reduce(state, { type: "activity/read", ids: ["e-1"] })).toBe(state);
  });

  it("a feed row jumps across projects, which selection/session cannot", () => {
    /** The event the fixture puts in another project on purpose. */
    const state = reduce(attached(), {
      type: "selection/jump",
      projectId: "p-api-gateway",
      sessionId: "s-rotate-jwt-secret",
    });
    expect(state.selection).toEqual({
      projectId: "p-api-gateway",
      sessionId: "s-rotate-jwt-secret",
      terminalId: "t-rotate-agent",
    });
  });

  it("a jump to a session that has aged out does nothing", () => {
    /** The host retains 24h of events, so a row can outlive its session.
     * Inventing a selection would move the desktop somewhere nobody asked for. */
    const before = attached();
    const after = reduce(before, {
      type: "selection/jump",
      projectId: "p-flightdeck",
      sessionId: "s-long-gone",
    });
    expect(after.selection).toBe(before.selection);
  });
});
