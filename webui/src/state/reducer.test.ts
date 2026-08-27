import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { createInitialState } from "./types";
import type { AppState } from "./types";

describe("createInitialState", () => {
  it("starts connecting, with no geometry and no queued input", () => {
    const state = createInitialState();
    expect(state).toEqual<AppState>({
      connection: "connecting",
      geometry: null,
      pendingInput: [],
    });
  });
});

describe("reduce", () => {
  it("connection/changed updates only the connection field", () => {
    const before = createInitialState();
    const after = reduce(before, {
      type: "connection/changed",
      status: "connected",
    });

    expect(after.connection).toBe("connected");
    expect(after.geometry).toBe(before.geometry);
    expect(after.pendingInput).toBe(before.pendingInput);
  });

  it("geometry/set stores the host-owned grid size (D4)", () => {
    const before = createInitialState();
    const after = reduce(before, {
      type: "geometry/set",
      geometry: { cols: 120, rows: 34 },
    });

    expect(after.geometry).toEqual({ cols: 120, rows: 34 });
  });

  it("input/queue appends to pendingInput in order, without dropping", () => {
    let state = createInitialState();
    state = reduce(state, { type: "input/queue", data: "a" });
    state = reduce(state, { type: "input/queue", data: "b" });
    state = reduce(state, { type: "input/queue", data: "c" });

    expect(state.pendingInput).toEqual(["a", "b", "c"]);
  });

  it("input/flush drains pendingInput", () => {
    let state = createInitialState();
    state = reduce(state, { type: "input/queue", data: "held while stale" });
    expect(state.pendingInput).toHaveLength(1);

    state = reduce(state, { type: "input/flush" });
    expect(state.pendingInput).toEqual([]);
  });

  it("input/flush on an already-empty queue returns the same state (no-op)", () => {
    const before = createInitialState();
    const after = reduce(before, { type: "input/flush" });

    expect(after).toBe(before);
  });

  it("never mutates the input state object", () => {
    const before = createInitialState();
    const beforeCopy = { ...before, pendingInput: [...before.pendingInput] };

    reduce(before, { type: "connection/changed", status: "disconnected" });
    reduce(before, { type: "input/queue", data: "x" });

    expect(before).toEqual(beforeCopy);
  });
});
