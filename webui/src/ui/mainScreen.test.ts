/**
 * @vitest-environment jsdom
 *
 * The main screen, rendered from the fixture and asserted region by region
 * against artboard `1a — MAIN, TERMINAL MODE` (plus 1b for App mode and 1c for
 * split). Everything renderable is assertable: no snapshot files, no
 * screenshots, just the strings and the structure the design specifies.
 *
 * xterm.js is not involved — `mount` is injected, which is the whole reason
 * `createTerminalStage` takes it as an argument. What *is* asserted about the
 * terminal is the part D4 cares about: that it is constructed with the host's
 * `cols`/`rows`, once.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fixtureSnapshot } from "../state/fixture";
import { createApp } from "./app";
import type { App } from "./app";
import type { TerminalGeometry } from "../state/types";

interface Harness {
  readonly app: App;
  readonly mounts: { geometry: TerminalGeometry; terminalId: string }[];
  readonly disposes: string[];
  q: (selector: string) => HTMLElement;
  all: (selector: string) => readonly HTMLElement[];
  text: (selector: string) => string;
}

function render(options: { readonly now?: () => number } = {}): Harness {
  const mounts: { geometry: TerminalGeometry; terminalId: string }[] = [];
  const disposes: string[] = [];
  const app = createApp({
    mount: (container, geometry, terminalId) => {
      mounts.push({ geometry, terminalId });
      container.append(`[${terminalId}]`);
      return () => disposes.push(terminalId);
    },
    ...(options.now === undefined ? {} : { now: options.now }),
  });
  document.body.append(app.el);

  app.store.dispatch({ type: "snapshot/received", snapshot: fixtureSnapshot() });
  app.store.dispatch({ type: "connection/changed", status: "connected" });
  app.store.dispatch({ type: "mode/set", mode: "terminal" });

  const q = (selector: string): HTMLElement => {
    const found = app.el.querySelector<HTMLElement>(selector);
    if (found === null) {
      throw new Error(`no element matched ${selector}`);
    }
    return found;
  };

  return {
    app,
    mounts,
    disposes,
    q,
    all: (selector) => [...app.el.querySelectorAll<HTMLElement>(selector)],
    text: (selector) => q(selector).textContent ?? "",
  };
}

beforeEach(() => {
  document.body.replaceChildren();
});

describe("the seven regions", () => {
  it("renders all seven, in the order 1a stacks them", () => {
    const h = render();
    const regions = [
      ".fd-logo",
      ".fd-projects",
      ".fd-sidebar",
      ".fd-tabs",
      ".fd-pane",
      ".fd-gitbar",
      ".fd-statusbar",
    ];
    for (const region of regions) {
      expect(h.app.el.querySelector(region), region).not.toBeNull();
    }
    /** Document order, not just presence: the git bar is above the status bar
     * and both are below the body, which is what makes the frame read as a
     * cockpit rather than a page. */
    const order = [...h.app.el.querySelectorAll<HTMLElement>(regions.join(","))]
      .map((el) => regions.find((r) => el.matches(r)));
    expect(order).toEqual(regions);
  });
});

describe("region 1 — logo band", () => {
  it("draws the wordmark and hides the ramps from assistive tech", () => {
    const h = render();
    expect(h.text(".fd-logo__word")).toBe("· FLIGHTDECK ·");
    const ramps = h.all(".fd-logo__ramp");
    expect(ramps).toHaveLength(2);
    for (const ramp of ramps) {
      expect(ramp.getAttribute("aria-hidden")).toBe("true");
    }
  });
});

describe("region 2 — project tab row", () => {
  it("renders 1a's three projects with their status dots", () => {
    const h = render();
    const tabs = h.all(".fd-project__select");
    expect(tabs.map((t) => t.textContent)).toEqual([
      "⠿flightdeck",
      "●api-gateway",
      "●web",
    ]);
    /** flightdeck has work in progress, so its glyph moves. */
    expect(
      h.q('.fd-project[data-selected="true"] .fd-glyph--spinner'),
    ).not.toBeNull();
  });

  it("marks exactly one tab selected, as a real tab role", () => {
    const h = render();
    const selected = h.all('.fd-project__select[aria-selected="true"]');
    expect(selected).toHaveLength(1);
    expect(selected[0]?.textContent).toContain("flightdeck");
    expect(selected[0]?.tagName).toBe("BUTTON");
  });

  it("badges + project as host only (D16) rather than hiding it", () => {
    const h = render();
    const action = h.q(".fd-action--project");
    expect(action.textContent).toContain("+ project");
    expect(h.text(".fd-action--project .fd-badge-host")).toBe("host only");
  });
});

describe("region 3 — agents sidebar", () => {
  it("renders 1a's six sessions, in order", () => {
    const h = render();
    expect(h.all(".fd-session__name").map((n) => n.textContent)).toEqual([
      "fix-login-redirect",
      "add-tests-api",
      "migrate-schema-v4",
      "flaky-e2e-runner",
      "perf-audit-images",
      "hotfix-csp-header",
    ]);
  });

  it("renders the selected session's three-line block exactly", () => {
    const h = render();
    const row = h.q('.fd-session[data-selected="true"]');
    expect(row.textContent).toContain("fix-login-redirect");
    expect(row.textContent).toContain("Claude Code [in progress]");
    expect(row.textContent).toContain("~dirty");
    expect(row.textContent).toContain("+3 -0");
    expect(row.textContent).toContain("drift:3");
  });

  it("renders the other five status lines from 1a", () => {
    const h = render();
    const rows = h.all(".fd-session").map((r) => r.textContent ?? "");
    expect(rows[1]).toContain("OpenCode [idle]");
    expect(rows[1]).toContain("no-upstream");
    expect(rows[2]).toContain("Codex CLI [waiting]");
    expect(rows[3]).toContain("Claude Code [error]");
    expect(rows[3]).toContain("[recovered]");
    expect(rows[3]).toContain("drift:7");
    expect(rows[4]).toContain("Claude Code [reviewing]");
    expect(rows[4]).toContain("·set");
    expect(rows[4]).toContain("really:idle");
    /** A session with no agent yet gets prose, not an invented status. */
    expect(rows[5]).toContain("creating worktree…");
    expect(rows[5]).toContain("git: ?");
    expect(rows[5]).not.toContain("[idle]");
  });

  it("puts every actionable dim string on the lifted floor, not on decor", () => {
    const h = render();
    /** 2g: if deleting it would lose a fact, it cannot be --fd-text-decor. */
    for (const factual of ["no-upstream", "git: ?"]) {
      const span = h
        .all(".fd-session__facts span")
        .find((s) => s.textContent === factual);
      expect(span, factual).toBeDefined();
      expect(span?.className).toBe("fd-tone-quiet");
    }
  });

  it("footers the count and the way to make another agent", () => {
    const h = render();
    expect(h.text(".fd-sidebar__foot")).toBe("6 sessions · Ctrl-g → “new agent”");
  });

  it("makes each row a real focusable control that says selection is shared", () => {
    const h = render();
    const button = h.all(".fd-session__select")[0];
    expect(button?.tagName).toBe("BUTTON");
    /** D3: the UI must never imply a browser-local selection. */
    expect(button?.title).toContain("also moves the desktop");
  });

  it("selects a session for the whole instance when clicked (D3)", () => {
    const h = render();
    const actions: string[] = [];
    h.app.store.subscribe(() => {
      actions.push(h.app.store.getState().selection?.sessionId ?? "");
    });
    h.all(".fd-session__select")[2]?.click();
    expect(h.app.store.getState().selection?.sessionId).toBe(
      "s-migrate-schema-v4",
    );
    expect(h.q('.fd-session[data-selected="true"]').textContent).toContain(
      "migrate-schema-v4",
    );
  });
});

describe("unknown stays unknown (§5.1)", () => {
  it("renders ○ and the full no-lifecycle line, never a guessed status", () => {
    const h = render();
    h.app.store.dispatch({
      type: "selection/project",
      projectId: "p-api-gateway",
    });

    const row = h.all(".fd-session")[0];
    const text = row?.textContent ?? "";
    expect(text).toContain("sync-openapi-types");
    expect(text).toContain("unknown → unknown · Codex CLI reports no lifecycle");
    /** The guess this requirement exists to forbid. */
    expect(text).not.toContain("[idle]");
    expect(text).not.toContain("[waiting]");
    expect(row?.querySelector(".fd-glyph")?.textContent).toBe("○");
    expect(row?.querySelector(".fd-glyph--spinner")).toBeNull();
  });
});

describe("region 4 — terminal tab bar", () => {
  it("renders 1a's three tabs with the agent tab selected", () => {
    const h = render();
    expect(h.all(".fd-tab__label").map((t) => t.textContent)).toEqual([
      "agent",
      "shell 1",
      "shell 2",
    ]);
    const selected = h.q('.fd-tab[data-selected="true"]');
    expect(selected.getAttribute("data-kind")).toBe("agent");
    expect(selected.textContent).toContain("agent");
  });

  it("keeps + agent and + shell visible but says they are M2", () => {
    const h = render();
    expect(h.text(".fd-action--new-agent")).toBe("+ agent");
    expect(h.text(".fd-action--new-shell")).toBe("+ shell");
    expect(h.q(".fd-action--new-agent").title).toContain("M2");
  });

  it("switching tab moves the shared selection", () => {
    const h = render();
    h.all(".fd-tab__label")[1]?.click();
    expect(h.app.store.getState().selection?.terminalId).toBe("t-shell-1");
  });
});

describe("region 5 — terminal viewport (D4: letterbox, never scale)", () => {
  it("constructs the terminal with the host's grid, verbatim", () => {
    const h = render();
    expect(h.mounts).toEqual([
      { geometry: { cols: 120, rows: 34 }, terminalId: "t-agent" },
    ]);
  });

  it("does not remount when unrelated state changes", () => {
    const h = render();
    h.app.store.dispatch({ type: "connection/changed", status: "catching_up" });
    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    expect(h.mounts).toHaveLength(1);
  });

  it("remounts, and disposes, only when the host changes the terminal", () => {
    const h = render();
    h.app.store.dispatch({ type: "selection/terminal", terminalId: "t-shell-1" });
    expect(h.mounts.map((m) => m.terminalId)).toEqual(["t-agent", "t-shell-1"]);
    expect(h.disposes).toEqual(["t-agent"]);
  });

  it("centres the grid on the stage instead of stretching it", () => {
    const h = render();
    /** The letterbox is a natural-size box inside a centring stage; nothing in
     * the app is allowed to give it a percentage size or a transform. */
    expect(h.q(".fd-stage .fd-letterbox .fd-mount")).not.toBeNull();
    expect(h.q(".fd-mount").getAttribute("style")).toBeNull();
  });

  it("sleeps the terminal in App mode and says how to wake it (1b/2d)", () => {
    const h = render();
    expect(h.q(".fd-pane").getAttribute("data-tone")).toBe("live");

    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    expect(h.q(".fd-pane").getAttribute("data-tone")).toBe("asleep");
    expect(h.text(".fd-pane__foot")).toBe(
      "terminal asleep — keystrokes go to FlightDeck · Enter or click to wake it",
    );
    /** Asleep is not stale: nothing here claims the picture is frozen. */
    expect(h.text(".fd-pane__foot")).not.toContain("frozen");
  });
});

describe("region 6 — git info bar", () => {
  it("renders 1a's branch and counts", () => {
    const h = render();
    const bar = h.text(".fd-gitbar");
    expect(bar).toContain("flightdeck/fix-login-redirect");
    expect(bar).toContain("+3");
    expect(bar).toContain("~2");
    expect(bar).toContain("-1");
    expect(bar).toContain("(6 files)");
    expect(bar).toContain("↑3 ↓0");
    expect(bar).toContain("base +4");
    expect(bar).toContain("base: main");
  });

  it("shows the host's geometry in the chip, not the browser's (D4)", () => {
    const h = render();
    expect(h.text(".fd-geometry")).toBe("120×34 · host owns geometry");
  });

  it("tracks the selected session's geometry chip when the host resizes", () => {
    const h = render();
    h.app.store.dispatch({
      type: "geometry/set",
      geometry: { cols: 80, rows: 24 },
    });
    expect(h.text(".fd-geometry")).toBe("80×24 · host owns geometry");
  });

  it("says git: ? rather than zeroes when git has not answered", () => {
    const h = render();
    h.app.store.dispatch({
      type: "selection/session",
      sessionId: "s-hotfix-csp-header",
    });
    expect(h.text(".fd-gitbar")).toContain("git: ?");
    expect(h.text(".fd-gitbar")).not.toContain("+0");
  });
});

describe("region 7 — status bar", () => {
  it("renders 1a's mode chip, hints, connection, viewers and update chip", () => {
    const h = render();
    expect(h.text(".fd-mode")).toBe("MODE: TERMINAL");
    const bar = h.text(".fd-statusbar");
    expect(bar).toContain("Esc Esc app commands");
    expect(bar).toContain("Ctrl-g command palette");
    expect(bar).toContain("click outside release keys");
    expect(bar).toContain("connected");
    expect(bar).toContain("18ms");
    /**
     * 1a drew `2 viewers (this tab + desktop)`. **Turn 2 supersedes it**: 2c
     * and 2f both draw `desktop + this tab` — "two named seats, not a counter
     * that implies a crowd" — and the reason a second seat is not alarming is
     * that you can *see* it is your own desktop, which a number cannot show.
     */
    expect(bar).toContain("desktop + this tab");
    expect(bar).not.toContain("2 viewers");
    expect(h.text(".fd-update")).toContain("v1.16.0 available");
  });

  it("swaps the hints for 1b's in App mode", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    expect(h.text(".fd-mode")).toBe("MODE: APP");
    expect(h.q(".fd-mode").getAttribute("data-tone")).toBe("app");
    expect(h.text(".fd-statusbar")).toContain("Enter focus terminal");
  });

  it("drains the chip the moment control is lost (§5.1)", () => {
    const h = render();
    h.app.store.dispatch({ type: "connection/changed", status: "reconnecting" });
    expect(h.text(".fd-mode")).toBe("MODE: —");
    expect(h.q(".fd-mode").getAttribute("data-tone")).toBe("drained");
    /** And it says what is happening to the keys you are still typing. */
    expect(h.text(".fd-statusbar")).toContain("keystrokes are being held");
  });
});

describe("the keyboard and pointer positions (§5)", () => {
  it("Esc Esc within 400 ms leaves terminal focus", () => {
    let clock = 1_000;
    const h = render({ now: () => clock });
    expect(h.app.store.getState().mode).toBe("terminal");

    press(h, "Escape");
    clock = 1_300;
    press(h, "Escape");

    expect(h.app.store.getState().mode).toBe("app");
    /** The frame's own attribute is what turns 1a into 1b, so assert it too. */
    expect(h.app.el.getAttribute("data-mode")).toBe("app");
  });

  it("a single Esc passes through to the agent and keeps focus", () => {
    let clock = 1_000;
    const h = render({ now: () => clock });
    press(h, "Escape");
    clock = 9_999;
    press(h, "Escape");

    const state = h.app.store.getState();
    expect(state.mode).toBe("terminal");
    /** Both escapes reached the agent; neither was eaten by the UI. */
    expect(state.pendingInput).toEqual(["\x1b", "\x1b"]);
  });

  it("clicking outside the terminal releases the keys", () => {
    const h = render();
    h.q(".fd-sidebar__title").dispatchEvent(
      new MouseEvent("click", { bubbles: true }),
    );
    expect(h.app.store.getState().mode).toBe("app");
  });

  it("clicking the terminal wakes it again", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    h.q(".fd-mount").dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(h.app.store.getState().mode).toBe("terminal");
  });

  it("claims Ctrl-g and nothing else", () => {
    const h = render();
    const ctrlG = new KeyboardEvent("keydown", {
      key: "g",
      ctrlKey: true,
      bubbles: true,
      cancelable: true,
    });
    h.app.el.dispatchEvent(ctrlG);
    expect(ctrlG.defaultPrevented).toBe(true);

    /** Every other chord belongs to the browser (§5: palette-primary). */
    for (const key of ["b", "k", "p"]) {
      const event = new KeyboardEvent("keydown", {
        key,
        ctrlKey: true,
        bubbles: true,
        cancelable: true,
      });
      h.app.el.dispatchEvent(event);
      expect(event.defaultPrevented, key).toBe(false);
    }
  });
});

describe("artboard 1c — split view", () => {
  it("replaces the tab bar with one labelled column per terminal", () => {
    const h = render();
    h.app.store.dispatch({ type: "layout/set", layout: "split" });

    expect(h.q(".fd-tabs").hidden).toBe(true);
    expect(h.q(".fd-pane").hidden).toBe(true);
    const labels = h.all(".fd-column__label").map((l) => l.textContent);
    expect(labels).toEqual(["agent1/3", "shell 12/3", "shell 23/3"]);
  });

  it("letterboxes each column from the host's grid, once per terminal", () => {
    const h = render();
    h.app.store.dispatch({ type: "layout/set", layout: "split" });
    expect(h.mounts.map((m) => m.terminalId)).toEqual([
      "t-agent",
      "t-agent",
      "t-shell-1",
      "t-shell-2",
    ]);
    for (const mount of h.mounts) {
      expect(mount.geometry).toEqual({ cols: 120, rows: 34 });
    }
  });

  it("tracks the focused column and its hints", () => {
    const h = render();
    h.app.store.dispatch({ type: "layout/set", layout: "split" });
    expect(h.all('.fd-column[data-focused="true"]')).toHaveLength(1);

    h.app.store.dispatch({ type: "split/focus", column: 2 });
    const focused = h.all(".fd-column").map((c) => c.getAttribute("data-focused"));
    expect(focused).toEqual(["false", "false", "true"]);
    expect(h.text(".fd-statusbar")).toContain("SPLIT 3 terminals");
    expect(h.text(".fd-statusbar")).toContain("←/→ move focus");
  });

  it("keeps the git bar and its geometry chip — those are still facts", () => {
    const h = render();
    h.app.store.dispatch({ type: "layout/set", layout: "split" });
    expect(h.text(".fd-geometry")).toBe("120×34 · host owns geometry");
  });
});

describe("accessibility and semantics", () => {
  it("uses real, focusable controls for everything clickable", () => {
    const h = render();
    const clickable = h.all(
      ".fd-session__select, .fd-project__select, .fd-tab__label",
    );
    expect(clickable.length).toBeGreaterThan(8);
    for (const control of clickable) {
      expect(control.tagName).toBe("BUTTON");
      expect(control.getAttribute("disabled")).toBeNull();
    }
  });

  it("gives the tab rows and the sidebar real roles and names", () => {
    const h = render();
    expect(h.q(".fd-projects").getAttribute("role")).toBe("tablist");
    expect(h.q(".fd-projects").getAttribute("aria-label")).toBe("Projects");
    expect(h.q(".fd-tabs").getAttribute("role")).toBe("tablist");
    expect(h.q(".fd-sidebar").getAttribute("aria-label")).toBe("Agents");
    expect(h.q(".fd-sessions").tagName).toBe("UL");
    expect(h.q(".fd-statusbar").getAttribute("role")).toBe("status");
  });

  it("names every ✕ so it is not six identical buttons in a screen reader", () => {
    const h = render();
    const labels = h
      .all(".fd-session__close")
      .map((c) => c.getAttribute("aria-label"));
    expect(labels[0]).toBe("Close session fix-login-redirect");
    expect(new Set(labels).size).toBe(labels.length);
  });

  it("does not offer a light theme (dark-only by decision)", () => {
    const h = render();
    expect(h.app.el.querySelector("[data-theme='light']")).toBeNull();
  });
});

describe("the store is the only state (D15)", () => {
  it("re-renders the whole screen from a fresh snapshot", () => {
    const h = render();
    const spy = vi.fn();
    h.app.store.subscribe(spy);
    h.app.store.dispatch({
      type: "snapshot/received",
      snapshot: fixtureSnapshot(),
    });
    expect(spy).toHaveBeenCalledOnce();
    expect(h.all(".fd-session__name")).toHaveLength(6);
  });

  it("hands every action to the wire seam, in order (D3)", () => {
    const seen: string[] = [];
    const app = createApp({
      mount: () => undefined,
      onDispatch: (action) => seen.push(action.type),
    });
    document.body.append(app.el);
    app.store.dispatch({
      type: "snapshot/received",
      snapshot: fixtureSnapshot(),
    });
    app.store.dispatch({ type: "selection/session", sessionId: "s-add-tests-api" });
    expect(seen).toEqual(["snapshot/received", "selection/session"]);
  });
});

function press(h: Harness, key: string): void {
  h.app.el.dispatchEvent(
    new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
  );
}
