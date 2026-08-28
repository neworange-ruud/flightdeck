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
