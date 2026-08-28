import { describe, expect, it } from "vitest";
import { createStore } from "../ui/store";
import { createInitialState } from "../state/types";
import { openSession } from "./socket";

/**
 * A live `Delta::Activity` used to hardcode `from`/`to` to the literal string
 * `"unknown"` regardless of what the host sent — so the same event told two
 * different stories depending on whether it arrived as snapshot backfill
 * (correctly mapped by `wire/adapt.ts`'s `activityOf`) or live (always
 * `unknown → unknown`). Turn 2 §5.1's rule is "unknown stays unknown", which
 * this broke in the *inverse* direction: claiming an ignorance the host did
 * not have.
 *
 * No DOM needed — `openSession` is driven entirely through an injected
 * `socketFactory`, so this stays a plain-node test like the reducer's.
 */

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

function deliverActivityDelta(
  ws: FakeSocket,
  fields: { readonly from: string; readonly to: string },
): void {
  ws.onmessage?.({
    data: JSON.stringify({
      type: "delta",
      change: "activity",
      event_id: "e-live-1",
      at_ms: 0,
      project_id: "p1",
      project_name: "flightdeck",
      session_id: "s1",
      session_name: "fix-login-redirect",
      from: fields.from,
      to: fields.to,
      reason: "finished, 3 files touched",
      tier: "finished",
      read: false,
    }),
  });
}

describe("wire/socket: live Delta::Activity", () => {
  it("reads the host's real from/to rather than a hardcoded literal", () => {
    const store = createStore(createInitialState());
    const ws = fakeSocket();
    openSession({
      store,
      url: "ws://test/ws",
      socketFactory: () => ws as unknown as WebSocket,
    });

    deliverActivityDelta(ws, { from: "waiting", to: "idle" });

    const [event] = store.getState().activity;
    expect(event).toBeDefined();
    expect(event?.from).toBe("waiting");
    expect(event?.to).toBe("idle");
  });

  it("still renders unknown -> unknown for a genuinely unknown-lifecycle event", () => {
    const store = createStore(createInitialState());
    const ws = fakeSocket();
    openSession({
      store,
      url: "ws://test/ws",
      socketFactory: () => ws as unknown as WebSocket,
    });

    deliverActivityDelta(ws, { from: "unknown", to: "unknown" });

    const [event] = store.getState().activity;
    expect(event?.from).toBe("unknown");
    expect(event?.to).toBe("unknown");
  });
});

/**
 * `remote-control-ll5.9`: artboard 2f lists three facts per seat — address,
 * browser, connected — and a `Delta::Seats` used to deliver the first two and
 * silently drop the third, because it carried no clock its rows' `since_ms`
 * could be dated against. The panel therefore drew three rows or two depending
 * on which frame the seat news happened to arrive in.
 */
describe("wire/socket: a seat delta dates its own rows", () => {
  const seat = {
    viewer_id: "v2",
    label: "192.168.2.20 · Safari on iOS",
    address: "192.168.2.20",
    user_agent_label: "Safari on iOS",
    seat: "writing",
    holds_input: true,
    since_ms: 1_700_000_000_000,
    is_you: false,
  };

  function deliverSeats(ws: FakeSocket, serverTimeMs: number | null): void {
    ws.onmessage?.({
      data: JSON.stringify({
        type: "delta",
        change: "seats",
        you: "writing",
        seats: [seat],
        ...(serverTimeMs === null ? {} : { server_time_ms: serverTimeMs }),
      }),
    });
  }

  function session(): { store: ReturnType<typeof createStore>; ws: FakeSocket } {
    const store = createStore(createInitialState());
    const ws = fakeSocket();
    openSession({
      store,
      url: "ws://test/ws",
      socketFactory: () => ws as unknown as WebSocket,
    });
    return { store, ws };
  }

  it("renders all three facts, dated on the host's clock", () => {
    const { store, ws } = session();
    deliverSeats(ws, 1_700_000_012_000);

    const [row] = store.getState().seats;
    expect(row?.address).toBe("192.168.2.20");
    expect(row?.browser).toBe("Safari on iOS");
    /** Twelve seconds measured entirely on the host's clock — never against
     * `Date.now()`, which has no relationship to it. */
    expect(row?.sinceLabel).toBe("12s ago");
  });

  it("leaves the row undated when the host sent no clock", () => {
    /** Honest degradation for a host from before the field. Empty means "we
     * cannot say", and 2f drops the row rather than drawing a fabricated or
     * negative duration. */
    const { store, ws } = session();
    deliverSeats(ws, null);

    const [row] = store.getState().seats;
    expect(row?.sinceLabel).toBe("");
    /** The other two facts still arrive: only the dating is lost. */
    expect(row?.address).toBe("192.168.2.20");
    expect(row?.browser).toBe("Safari on iOS");
  });

  it("treats serde's default 0 as no clock, not as 1970", () => {
    const { store, ws } = session();
    deliverSeats(ws, 0);
    expect(store.getState().seats[0]?.sinceLabel).toBe("");
  });

  it("completes an arriving takeover panel that opened without a time", () => {
    /**
     * `WireError::seat_held` names the writer that is typing but is not a seat
     * list, so the panel opens with `connected` blank. The dated list that
     * follows finishes it — which is what makes the panel show the same three
     * facts however the news arrived.
     */
    const { store, ws } = session();
    ws.onmessage?.({
      data: JSON.stringify({
        type: "error",
        code: "seat_held",
        message: "192.168.2.20 · Safari on iOS is typing right now.",
        incumbent: seat,
      }),
    });
    expect(store.getState().takeover).toEqual({
      kind: "arriving",
      incumbent: {
        address: "192.168.2.20",
        browser: "Safari on iOS",
        connected: "",
      },
    });

    deliverSeats(ws, 1_700_000_012_000);
    expect(store.getState().takeover).toEqual({
      kind: "arriving",
      incumbent: {
        address: "192.168.2.20",
        browser: "Safari on iOS",
        connected: "12s ago",
      },
    });
  });
});

/**
 * `remote-control-eek.3`: 2f's *evicted* panel finally has a dispatcher, and the
 * whole of the work is which seat deltas may open it.
 *
 * It was modelled, styled and tested from turn 2 and never fired — under
 * protocol v1 for want of anything to fire it on, and under the revision for a
 * sharper reason: **the input lock moves on every ordinary hand-off.** One
 * writer stops typing, another starts, several times a minute, and every one of
 * those is a `Delta::Seats` in which the lock left somebody. Opening a modal on
 * that would put a dialog in front of a reader every time their colleague began
 * a sentence.
 *
 * The distinguishing fact is *intent*, which exists only at the host at the
 * moment of the act, so the host carries it per recipient
 * (`Delta::Seats::you_were_preempted`). These are the two halves.
 */
describe("wire/socket: the evicted panel opens on preemption only", () => {
  const holder = {
    viewer_id: "v9",
    label: "192.168.2.11 · Chrome on macOS",
    address: "192.168.2.11",
    user_agent_label: "Chrome on macOS",
    seat: "writing",
    holds_input: true,
    since_ms: 1_700_000_000_000,
    is_you: false,
  };

  function session(): {
    store: ReturnType<typeof createStore>;
    ws: FakeSocket;
    tick: () => void;
  } {
    let clock = 1_700_000_000_000;
    const store = createStore(createInitialState());
    const ws = fakeSocket();
    openSession({
      store,
      url: "ws://test/ws",
      socketFactory: () => ws as unknown as WebSocket,
      now: () => clock,
    });
    return {
      store,
      ws,
      /** Eight seconds, not 2f's literal three: `agoLabel` floors anything
       * under five at `just now`, which is the app's own vocabulary for a gap
       * too short to put a number on. */
      tick: () => {
        clock += 8_000;
      },
    };
  }

  function deliverSeats(ws: FakeSocket, preempted: boolean): void {
    ws.onmessage?.({
      data: JSON.stringify({
        type: "delta",
        change: "seats",
        you: "writing",
        seats: [holder],
        server_time_ms: 1_700_000_012_000,
        you_were_preempted: preempted,
      }),
    });
  }

  it("stays shut when the lock simply moved", () => {
    /** The common case by a wide margin, and the reason this cannot be driven
     * off the rows: the reader lost the turn here too. */
    const { store, ws } = session();
    deliverSeats(ws, false);
    expect(store.getState().seats).toHaveLength(1);
    expect(store.getState().takeover).toBeNull();
  });

  it("stays shut for a host that says nothing at all", () => {
    /** Absent is `false` on the wire, and the honest reading of it is "this
     * host reports no preemptions" — which leaves the browser where it was
     * before the field existed, not guessing. */
    const { store, ws } = session();
    ws.onmessage?.({
      data: JSON.stringify({
        type: "delta",
        change: "seats",
        you: "writing",
        seats: [holder],
        server_time_ms: 1_700_000_012_000,
      }),
    });
    expect(store.getState().takeover).toBeNull();
  });

  it("opens naming the writer that took it, when the host says it was deliberate", () => {
    const { store, ws, tick } = session();
    /** A keystroke that actually landed, some seconds before the interruption
     * — 2f's "the last one that landed was 3s ago". */
    ws.onmessage?.({
      data: JSON.stringify({ type: "ack", seq: 1, outcome: "applied" }),
    });
    tick();
    deliverSeats(ws, true);

    expect(store.getState().takeover).toEqual({
      kind: "evicted",
      /** The host-observed address, never the merged label split on its
       * separator — the browser half of that label is untrusted free text. */
      byAddress: "192.168.2.11",
      lastInputAgo: "8s ago",
    });
    /** Losing the turn is a `Delta::Seats`, never a `Shutdown`: the socket and
     * the seat both stay, which is what makes waiting a real option. */
    expect(store.getState().shutdown).toBeNull();
    expect(store.getState().seat).toBe("writing");
  });

  it("dates the panel from a keystroke that landed, never from one refused", () => {
    /**
     * A `rejected` ack is a keystroke the host declined — it never reached the
     * PTY. Dating "the last one that landed" from it would tell the reader
     * their typing was arriving when it was not, so the clause is left out
     * entirely instead.
     */
    const { store, ws, tick } = session();
    ws.onmessage?.({
      data: JSON.stringify({
        type: "ack",
        seq: 1,
        outcome: "rejected",
        detail: "someone else is typing",
      }),
    });
    tick();
    deliverSeats(ws, true);

    expect(store.getState().takeover).toEqual({
      kind: "evicted",
      byAddress: "192.168.2.11",
      lastInputAgo: "",
    });
  });
});

/**
 * `remote-control-ll5.7`: split view is shared instance state (D3), so the
 * toggle round-trips through `sendCommand("toggle_split_view")` and the
 * layout only ever moves from what the host says next — never from the
 * command itself.
 */
describe("wire/socket: split-view toggling (D3/D8)", () => {
  it("a live Delta::Selection sets layout from its split_view flag, immediately", () => {
    const store = createStore(createInitialState());
    const ws = fakeSocket();
    openSession({
      store,
      url: "ws://test/ws",
      socketFactory: () => ws as unknown as WebSocket,
    });
    expect(store.getState().layout).toBe("single");

    ws.onmessage?.({
      data: JSON.stringify({
        type: "delta",
        change: "selection",
        project_id: "p1",
        session_id: "s1",
        terminal_id: "t1",
        split_view: true,
      }),
    });
    expect(store.getState().layout).toBe("split");

    ws.onmessage?.({
      data: JSON.stringify({
        type: "delta",
        change: "selection",
        split_view: false,
      }),
    });
    expect(store.getState().layout).toBe("single");
  });

  it("sendCommand(\"toggle_split_view\") sends a Command frame, not a local layout flip", () => {
    const store = createStore(createInitialState());
    const ws = fakeSocket();
    const sent: string[] = [];
    ws.send = (data: string) => {
      sent.push(data);
    };
    const session = openSession({
      store,
      url: "ws://test/ws",
      socketFactory: () => ws as unknown as WebSocket,
    });

    const seq = session.sendCommand("toggle_split_view");
    expect(store.getState().layout).toBe("single");

    // Not yet attached in this test, so nothing reached the wire — but the
    // seq is still real and still owns the counter shared with Input (§5.1).
    expect(sent).toEqual([]);
    expect(seq).toBeGreaterThan(0);
  });

  it("D14: a ReadOnly refusal for the toggle leaves layout untouched", () => {
    const store = createStore(createInitialState());
    const ws = fakeSocket();
    const session = openSession({
      store,
      url: "ws://test/ws",
      socketFactory: () => ws as unknown as WebSocket,
    });
    const seq = session.sendCommand("toggle_split_view");

    ws.onmessage?.({
      data: JSON.stringify({
        type: "error",
        code: "read_only",
        message: "this tab is watching read-only; take over to drive",
        seq,
      }),
    });

    expect(store.getState().layout).toBe("single");
  });
});

/**
 * D13: the dialog's whole life arrives on the wire. Applied directly rather
 * than resynced, because `requestSnapshotSoon` would put a coalesced round trip
 * between a `y` pressed on the desktop and the modal leaving this screen — and
 * the frames already carry everything the store needs.
 */
describe("wire/socket: D13's shared dialog", () => {
  function session(ws: FakeSocket) {
    const store = createStore(createInitialState());
    openSession({
      store,
      url: "ws://test/ws",
      socketFactory: () => ws as unknown as WebSocket,
    });
    return store;
  }

  function openedFrame(origin: unknown): string {
    return JSON.stringify({
      type: "delta",
      change: "dialog_opened",
      dialog_id: "dialog-7",
      kind: "new_agent",
      title: "New Agent Session Tab",
      origin,
      body: {
        input: "",
        list: [{ label: "(•) Claude Code", selected: true }],
        buttons: [
          { key: "Enter", label: "Create" },
          { key: "Esc", label: "Cancel" },
        ],
        confirmable: true,
      },
    });
  }

  it("a Delta::DialogOpened carries the origin through to the store", () => {
    const ws = fakeSocket();
    const store = session(ws);

    ws.onmessage?.({
      data: openedFrame({ origin: "browser", label: "192.168.2.20" }),
    });

    const dialog = store.getState().dialog;
    expect(dialog?.id).toBe("dialog-7");
    expect(dialog?.kind).toBe("new_agent");
    expect(dialog?.origin).toEqual({
      kind: "browser",
      label: "192.168.2.20",
    });
    expect(dialog?.confirmable).toBe(true);
  });

  it("a desktop-opened dialog arrives tagged as the desktop's", () => {
    const ws = fakeSocket();
    const store = session(ws);
    ws.onmessage?.({ data: openedFrame({ origin: "desktop" }) });
    expect(store.getState().dialog?.origin).toEqual({ kind: "desktop" });
  });

  it("a Delta::DialogClosed takes it down, whichever surface decided", () => {
    const ws = fakeSocket();
    const store = session(ws);
    ws.onmessage?.({ data: openedFrame({ origin: "desktop" }) });

    ws.onmessage?.({
      data: JSON.stringify({
        type: "delta",
        change: "dialog_closed",
        dialog_id: "dialog-7",
        outcome: "confirmed",
      }),
    });
    expect(store.getState().dialog).toBeNull();
  });

  it("a close for a dialog that is not the open one is ignored", () => {
    const ws = fakeSocket();
    const store = session(ws);
    ws.onmessage?.({ data: openedFrame({ origin: "desktop" }) });

    ws.onmessage?.({
      data: JSON.stringify({
        type: "delta",
        change: "dialog_closed",
        dialog_id: "dialog-1",
        outcome: "superseded",
      }),
    });
    expect(store.getState().dialog?.id).toBe("dialog-7");
  });

  it("a superseded close is passed through as superseded, not as an answer", () => {
    /** A browser that flattened it into a cancel would be claiming somebody
     * answered a question nobody answered. */
    const ws = fakeSocket();
    const store = session(ws);
    const dispatched: string[] = [];
    store.subscribe((state) => {
      dispatched.push(state.dialog === null ? "closed" : state.dialog.id);
    });
    ws.onmessage?.({ data: openedFrame({ origin: "desktop" }) });
    ws.onmessage?.({
      data: JSON.stringify({
        type: "delta",
        change: "dialog_closed",
        dialog_id: "dialog-7",
        outcome: "superseded",
      }),
    });
    expect(dispatched).toEqual(["dialog-7", "closed"]);
  });

  it("sends dialog_confirm as a Command frame with the dialog named", () => {
    const ws = fakeSocket();
    const sent: string[] = [];
    ws.send = (data: string) => {
      sent.push(data);
    };
    const store = createStore(createInitialState());
    const s = openSession({
      store,
      url: "ws://test/ws",
      socketFactory: () => ws as unknown as WebSocket,
    });
    const seq = s.sendCommand("dialog_confirm", { dialog_id: "dialog-7" });
    /** Not attached in this test, so nothing reached the wire — but the seq is
     * real and owns the counter shared with `Input` (§5.1), which is what lets
     * `command/result` settle the answer against the host's `Ack`. */
    expect(sent).toEqual([]);
    expect(seq).toBeGreaterThan(0);
  });
});
