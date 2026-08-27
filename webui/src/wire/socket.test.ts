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
