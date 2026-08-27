/**
 * @vitest-environment jsdom
 *
 * The configuration manager (1f, `remote-control-ll5.6`), rendered through
 * `createApp` exactly like `commandPalette.test.ts` renders 1d — real
 * elements, real keyboard events, no snapshot files.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { fixtureSnapshot } from "../state/fixture";
import { createApp } from "./app";
import type { App } from "./app";
import type { ConfigSaveRequest } from "../state/config";
import type { AppAction, AppState } from "../state/types";

interface Harness {
  readonly app: App;
  readonly dispatched: AppAction[];
  readonly saved: ConfigSaveRequest[];
  q: (selector: string) => HTMLElement;
  maybe: (selector: string) => HTMLElement | null;
  all: (selector: string) => readonly HTMLElement[];
  text: (selector: string) => string;
  state: () => AppState;
  key: (key: string, init?: KeyboardEventInit) => void;
}

function render(): Harness {
  const dispatched: AppAction[] = [];
  const saved: ConfigSaveRequest[] = [];
  const app = createApp({
    mount: (container, _geometry, terminalId) => {
      container.append(`[${terminalId}]`);
    },
    onDispatch: (action) => dispatched.push(action),
    onSaveConfig: (request) => saved.push(request),
  });
  document.body.append(app.el);

  app.store.dispatch({ type: "snapshot/received", snapshot: fixtureSnapshot() });
  app.store.dispatch({ type: "connection/changed", status: "connected" });
  app.store.dispatch({ type: "mode/set", mode: "terminal" });
  app.store.dispatch({ type: "host/set", host: "192.168.2.14:7420" });

  const q = (selector: string): HTMLElement => {
    const found = app.el.querySelector<HTMLElement>(selector);
    if (found === null) {
      throw new Error(`no element matched ${selector}`);
    }
    return found;
  };

  return {
    app,
    dispatched,
    saved,
    q,
    maybe: (selector) => app.el.querySelector<HTMLElement>(selector),
    all: (selector) => [...app.el.querySelectorAll<HTMLElement>(selector)],
    text: (selector) => q(selector).textContent ?? "",
    state: () => app.store.getState(),
    key: (key, init = {}) => {
      app.el.dispatchEvent(
        new KeyboardEvent("keydown", { key, bubbles: true, ...init }),
      );
    },
  };
}

/** Types a whole string through the palette's own keydown handler, one
 * `keydown` per character — exactly what a user typing does. */
function type(h: Harness, text: string): void {
  for (const char of text) {
    h.key(char);
  }
}

/** Opens the manager directly on the store — the palette route to the same
 * action is covered separately, below. */
function open(h: Harness): void {
  h.app.store.dispatch({ type: "config/open" });
}

function row(h: Harness, label: string): HTMLElement {
  const found = h
    .all(".fd-config__row")
    .find((r) => r.querySelector(".fd-config__label")?.textContent === label);
  if (found === undefined) {
    throw new Error(`no row labelled ${label}`);
  }
  return found;
}

function origin(h: Harness, label: string): string | null {
  return row(h, label).querySelector(".fd-config__origin")?.textContent ?? null;
}

function value(h: Harness, label: string): string | null {
  return row(h, label).querySelector(".fd-config__value")?.textContent ?? null;
}

beforeEach(() => {
  document.body.replaceChildren();
});

describe("opening and closing", () => {
  it("is closed to start with", () => {
    const h = render();
    expect(h.state().config).toBeNull();
    expect(h.q(".fd-config").hidden).toBe(true);
  });

  it("the palette's Open Configuration row opens it, and closes the palette", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    type(h, "configuration");
    const paletteRow = h.q(".fd-palette__row");
    expect(paletteRow.textContent).toContain("Open Configuration");
    paletteRow.click();

    expect(h.state().palette).toBeNull();
    expect(h.state().config).not.toBeNull();
    expect(h.q(".fd-config").hidden).toBe(false);
  });

  it("opens on Project scope by default, and Esc closes it", () => {
    const h = render();
    open(h);
    expect(h.state().config?.scope).toBe("project");
    h.key("Escape");
    expect(h.state().config).toBeNull();
    expect(h.q(".fd-config").hidden).toBe(true);
  });
});

describe("origin attribution — every combination the reducer can produce", () => {
  it("set here: a project override with nothing else beneath it", () => {
    const h = render();
    open(h);
    expect(origin(h, "Agent tab position")).toBe("(set here)");
    expect(value(h, "Agent tab position")).toBe("‹right›");
  });

  it("global only: no project override, the global file has an explicit value", () => {
    const h = render();
    open(h);
    expect(origin(h, "OS notifications")).toBe("(global)");
    expect(value(h, "OS notifications")).toBe("[x]");
  });

  it("default only: neither file has an explicit value", () => {
    const h = render();
    open(h);
    expect(origin(h, "Notification sounds")).toBe("(default)");
    expect(value(h, "Notification sounds")).toBe("[ ]");
  });

  it("set here, shadowing a global value underneath it", () => {
    const h = render();
    open(h);
    // The project override wins and reads exactly like any other "set here" —
    // the shadowed global value is not observable until the override clears.
    expect(origin(h, "Notify when finished")).toBe("(set here)");

    h.app.store.dispatch({
      type: "config/select",
      key: "notifications.on_finished",
    });
    h.key("c");

    // Clearing reveals the layer underneath: global, not straight to default.
    expect(origin(h, "Notify when finished")).toBe("(global)");
    expect(value(h, "Notify when finished")).toBe("[x]");
  });
});

describe("Tab switches scope", () => {
  it("moves between Project and Global and back", () => {
    const h = render();
    open(h);
    expect(h.state().config?.scope).toBe("project");
    h.key("Tab");
    expect(h.state().config?.scope).toBe("global");
    h.key("Tab");
    expect(h.state().config?.scope).toBe("project");
  });

  it("Global scope never shows a (global) tag — only set here or default", () => {
    const h = render();
    open(h);
    h.key("Tab");
    expect(h.state().config?.scope).toBe("global");
    const origins = h.all(".fd-config__origin").map((o) => o.textContent);
    expect(origins).not.toContain("(global)");
    // The field that was "(global)" in Project scope is the global file's own
    // explicit value from Global scope's point of view — "set here".
    expect(origin(h, "OS notifications")).toBe("(set here)");
  });
});

describe("c clears a project override", () => {
  it("the tag updates from set here to default", () => {
    const h = render();
    open(h);
    h.app.store.dispatch({ type: "config/select", key: "ui.app_mode_color" });
    expect(origin(h, "App mode color")).toBe("(set here)");

    h.key("c");

    expect(origin(h, "App mode color")).toBe("(default)");
    expect(value(h, "App mode color")).toBe("‹cyan›");
    expect(h.q(".fd-config__unsaved").hidden).toBe(false);
  });

  it("is a no-op in Global scope", () => {
    const h = render();
    open(h);
    h.key("Tab");
    h.app.store.dispatch({ type: "config/select", key: "ui.app_mode_color" });
    h.key("c");
    expect(Object.keys(h.state().config?.edits ?? {})).toHaveLength(0);
  });
});

describe("D5: the routable-bind warning", () => {
  it("appears when the resolved bind is routable", () => {
    const h = render();
    open(h);
    expect(value(h, "Web interface bind address")).toBe("0.0.0.0");
    const warnings = h.all(".fd-config__note--inline");
    expect(warnings).toHaveLength(1);
    expect(warnings[0]?.textContent).toContain("routable");
  });

  it("does not appear once the value resolves to loopback", () => {
    const h = render();
    open(h);
    h.app.store.dispatch({ type: "config/select", key: "web.bind" });
    h.key("c");
    expect(value(h, "Web interface bind address")).toBe("127.0.0.1");
    expect(h.all(".fd-config__note--inline")).toHaveLength(0);
  });
});

describe("D16: the editor action and the host-only row", () => {
  it("the footer's e edit in $EDITOR carries a host-only badge and is not hidden", () => {
    const h = render();
    open(h);
    const hintEl = h.q(".fd-config__editor-hint");
    expect(hintEl.hidden).toBe(false);
    expect(hintEl.textContent).toContain("edit in $EDITOR");
    expect(hintEl.querySelector(".fd-badge-host")?.textContent).toBe("host only");
  });

  it("the F2 row is rendered, not hidden, with a host-only origin and no value", () => {
    const h = render();
    open(h);
    const f2Row = row(
      h,
      "Use F2 to leave terminal focus · web: Esc Esc or click outside",
    );
    expect(f2Row.hidden).toBe(false);
    expect(f2Row.querySelector(".fd-config__value")?.textContent).toBe("—");
    expect(f2Row.querySelector(".fd-badge-host")?.textContent).toBe("host only");
  });

  it("e does nothing this build can act on — swallowed, not fabricated", () => {
    const h = render();
    open(h);
    const before = h.state().config;
    h.key("e");
    expect(h.state().config).toEqual(before);
  });
});

describe("saving follows the host's Ack, never optimism", () => {
  it("stages a clear locally, sends it on s, and only settles on the real Ack", () => {
    const h = render();
    open(h);
    h.app.store.dispatch({ type: "config/select", key: "ui.app_mode_color" });
    h.key("c");
    expect(h.q(".fd-config__unsaved").hidden).toBe(false);

    h.key("s");
    expect(h.saved).toHaveLength(1);
    expect(h.saved[0]).toEqual({
      scope: "project",
      changes: [{ key: "ui.app_mode_color", action: "clear" }],
    });
    // Sending is not applying: nothing has settled yet.
    expect(h.q(".fd-config__unsaved").hidden).toBe(false);
    expect(h.maybe(".fd-config__status")?.hidden ?? true).toBe(true);

    // What `main.ts` does once `sendCommand` returns a seq.
    h.app.store.dispatch({ type: "config/dispatched", seq: 11 });
    expect(h.q(".fd-config__status").hidden).toBe(false);
    expect(h.text(".fd-config__status")).toBe("Saving…");
    // Still not optimistic while the Ack is in flight.
    expect(h.q(".fd-config__unsaved").hidden).toBe(false);

    // The host's actual Ack, not a guess.
    h.app.store.dispatch({ type: "command/result", seq: 11, outcome: "applied" });
    expect(h.text(".fd-config__status")).toBe("Saved");
    expect(h.q(".fd-config__unsaved").hidden).toBe(true);
  });

  it("a rejected save keeps the staged edit instead of pretending it landed", () => {
    const h = render();
    open(h);
    h.app.store.dispatch({ type: "config/select", key: "ui.app_mode_color" });
    h.key("c");
    h.app.store.dispatch({ type: "config/dispatched", seq: 5 });
    h.app.store.dispatch({
      type: "command/result",
      seq: 5,
      outcome: "rejected",
      detail: "unknown command",
    });

    expect(h.text(".fd-config__status")).toBe("Save rejected — unknown command");
    expect(h.q(".fd-config__unsaved").hidden).toBe(false);
    expect(origin(h, "App mode color")).toBe("(default)");
  });

  it("s with nothing staged sends nothing", () => {
    const h = render();
    open(h);
    h.key("s");
    expect(h.saved).toHaveLength(0);
  });

  it("ignores a result whose seq nothing in the config manager is waiting on", () => {
    const h = render();
    open(h);
    h.app.store.dispatch({ type: "command/result", seq: 404, outcome: "applied" });
    expect(h.maybe(".fd-config__status")?.hidden ?? true).toBe(true);
  });
});
