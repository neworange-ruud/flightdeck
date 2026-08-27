import { describe, expect, it } from "vitest";
import { fixtureSnapshot } from "./fixture";
import { reduce } from "./reducer";
import { createInitialState } from "./types";
import type { AppState } from "./types";

describe("createInitialState", () => {
  it("starts connecting, with nothing it has not been told", () => {
    const state = createInitialState();
    expect(state).toEqual<AppState>({
      connection: "connecting",
      geometry: null,
      pendingInput: [],
      projects: [],
      selection: null,
      /** App mode is the honest default: the app cannot promise keystrokes
       * reach a PTY it has not heard about yet. */
      mode: "app",
      layout: "single",
      splitFocus: 0,
      viewers: 0,
      latencyMs: null,
      update: null,
      escArmedAt: null,
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

/**
 * The main screen's own reductions (remote-control-sk4u). Note what is *not*
 * here: no DOM, no fixture rendering, no clock. The `Esc Esc` window is
 * exercised through the action's `at` field, which is what keeps `reduce` pure
 * while still owning the behaviour.
 */
describe("reduce — the main screen", () => {
  const loaded = () =>
    reduce(createInitialState(), {
      type: "snapshot/received",
      snapshot: fixtureSnapshot(),
    });

  it("snapshot/received takes the whole picture, host selection included", () => {
    const state = loaded();
    expect(state.projects.map((p) => p.name)).toEqual([
      "flightdeck",
      "api-gateway",
      "web",
    ]);
    /** D3: the host's selection wins — a browser that kept its own would be a
     * second source of truth. */
    expect(state.selection).toEqual({
      projectId: "p-flightdeck",
      sessionId: "s-fix-login-redirect",
      terminalId: "t-agent",
    });
    expect(state.geometry).toEqual({ cols: 120, rows: 34 });
    expect(state.viewers).toBe(2);
    expect(state.latencyMs).toBe(18);
    expect(state.update).toEqual({ version: "v1.16.0" });
  });

  it("selection/session moves the instance-wide selection (D3)", () => {
    const state = reduce(loaded(), {
      type: "selection/session",
      sessionId: "s-migrate-schema-v4",
    });
    expect(state.selection?.sessionId).toBe("s-migrate-schema-v4");
    /** And onto that session's own agent terminal: a terminal id from the
     * previous session would point at nothing. */
    expect(state.selection?.terminalId).toBe("t-migrate-agent");
  });

  it("selection/project selects the project's first session and terminal", () => {
    const state = reduce(loaded(), {
      type: "selection/project",
      projectId: "p-api-gateway",
    });
    expect(state.selection).toEqual({
      projectId: "p-api-gateway",
      sessionId: "s-sync-openapi-types",
      terminalId: "t-sync-agent",
    });
  });

  it("ignores a selection of something the host never mentioned", () => {
    const before = loaded();
    expect(reduce(before, { type: "selection/project", projectId: "nope" })).toBe(
      before,
    );
    expect(
      reduce(before, { type: "selection/session", sessionId: "nope" }),
    ).toBe(before);
  });

  it("selection/terminal keeps project and session put", () => {
    const state = reduce(loaded(), {
      type: "selection/terminal",
      terminalId: "t-shell-2",
    });
    expect(state.selection).toEqual({
      projectId: "p-flightdeck",
      sessionId: "s-fix-login-redirect",
      terminalId: "t-shell-2",
    });
  });

  it("a lone Esc reaches the agent instead of being eaten (§5)", () => {
    const state = reduce(
      { ...loaded(), mode: "terminal" },
      { type: "input/esc", at: 1_000 },
    );
    expect(state.mode).toBe("terminal");
    expect(state.pendingInput).toEqual(["\x1b"]);
    expect(state.escArmedAt).toBe(1_000);
  });

  it("Esc Esc inside 400 ms leaves terminal focus and queues nothing more", () => {
    let state = reduce(
      { ...loaded(), mode: "terminal" },
      { type: "input/esc", at: 1_000 },
    );
    state = reduce(state, { type: "input/esc", at: 1_250 });
    expect(state.mode).toBe("app");
    /** The first Esc was already delivered; the second is the chord, not a
     * keystroke, so it is not sent. */
    expect(state.pendingInput).toEqual(["\x1b"]);
    expect(state.escArmedAt).toBeNull();
  });

  it("Esc Esc outside 400 ms sends both keys and stays in the terminal", () => {
    let state = reduce(
      { ...loaded(), mode: "terminal" },
      { type: "input/esc", at: 1_000 },
    );
    state = reduce(state, { type: "input/esc", at: 1_500 });
    expect(state.mode).toBe("terminal");
    expect(state.pendingInput).toEqual(["\x1b", "\x1b"]);
  });

  it("leaving terminal mode disarms a half-typed chord", () => {
    const armed = reduce(
      { ...loaded(), mode: "terminal" },
      { type: "input/esc", at: 1_000 },
    );
    const left = reduce(armed, { type: "mode/set", mode: "app" });
    expect(left.escArmedAt).toBeNull();
  });

  it("layout and split focus are state, not DOM", () => {
    const split = reduce(loaded(), { type: "layout/set", layout: "split" });
    expect(split.layout).toBe("split");
    expect(reduce(split, { type: "split/focus", column: 2 }).splitFocus).toBe(2);
  });
});
