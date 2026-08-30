/**
 * @vitest-environment jsdom
 *
 * The command palette (1d), rendered through `createApp` exactly like
 * `turn2Screens.test.ts` renders 2b–2f — real elements, real keyboard events,
 * no snapshot files.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { fixtureCommands, fixtureSnapshot } from "../state/fixture";
import { createApp } from "./app";
import type { App } from "./app";
import type { PaletteCommand } from "../state/commands";
import type { AppAction, AppState } from "../state/types";

interface Harness {
  readonly app: App;
  readonly dispatched: AppAction[];
  readonly runCommands: PaletteCommand[];
  q: (selector: string) => HTMLElement;
  maybe: (selector: string) => HTMLElement | null;
  all: (selector: string) => readonly HTMLElement[];
  text: (selector: string) => string;
  state: () => AppState;
  key: (key: string, init?: KeyboardEventInit) => void;
}

function render(): Harness {
  const dispatched: AppAction[] = [];
  const runCommands: PaletteCommand[] = [];
  const app = createApp({
    mount: (container, _geometry, terminalId) => {
      container.append(`[${terminalId}]`);
    },
    onDispatch: (action) => dispatched.push(action),
    onRunCommand: (command) => runCommands.push(command),
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
    runCommands,
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

/** The rendered row whose label is exactly `label` — the row text also
 * carries the right-hand tag, so a `textContent` match would confuse
 * `New Agent` with `New Agent Session Tab`. */
function rowLabelled(h: Harness, label: string): HTMLElement | undefined {
  return h
    .all(".fd-palette__row")
    .find((r) => r.querySelector(".fd-palette__label")?.textContent === label);
}

beforeEach(() => {
  document.body.replaceChildren();
});

describe("Ctrl-g: the only chord, opening and closing the palette", () => {
  it("is closed to start with", () => {
    const h = render();
    expect(h.state().palette).toBeNull();
    expect(h.q(".fd-palette").hidden).toBe(true);
  });

  it("opens on Ctrl-g and closes on a second Ctrl-g", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    expect(h.state().palette).not.toBeNull();
    expect(h.q(".fd-palette").hidden).toBe(false);

    h.key("g", { ctrlKey: true });
    expect(h.state().palette).toBeNull();
    expect(h.q(".fd-palette").hidden).toBe(true);
  });

  it("closes on Esc too", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    expect(h.state().palette).not.toBeNull();
    h.key("Escape");
    expect(h.state().palette).toBeNull();
  });

  it("is still the only chord bound: no other Ctrl-combination opens or affects it", () => {
    const h = render();
    for (const letter of ["b", "p", "k", "a"]) {
      h.key(letter, { ctrlKey: true });
    }
    expect(h.state().palette).toBeNull();

    h.key("g", { ctrlKey: true });
    expect(h.state().palette).not.toBeNull();
    // Once open, other Ctrl-combinations still do not close it or type into it.
    for (const letter of ["b", "p", "k"]) {
      h.key(letter, { ctrlKey: true });
    }
    expect(h.state().palette).not.toBeNull();
    expect(h.state().palette?.filter).toBe("");
  });
});

describe("filtering", () => {
  it("narrows the rows and highlights the matched substring in the label (1d)", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    const before = h.all(".fd-palette__row").length;

    type(h, "terminal");
    expect(h.state().palette?.filter).toBe("terminal");

    const after = h.all(".fd-palette__row").length;
    expect(after).toBeGreaterThan(0);
    expect(after).toBeLessThan(before);

    const match = h.q(".fd-palette__match");
    expect(match.textContent?.toLowerCase()).toBe("terminal");

    expect(h.text(".fd-palette__count")).toBe(`${after} of ${before} commands`);
  });

  it("Backspace narrows the filter back down", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    type(h, "session");
    expect(h.state().palette?.filter).toBe("session");
    h.key("Backspace");
    expect(h.state().palette?.filter).toBe("sessio");
  });
});

/**
 * D16, now with the badge coming off the wire (`remote-control-ll5.12`): the
 * host sets `host_only`, and these two rows are the ones it sets it on. The
 * selected row always shows `Enter` instead of its tag (1d), so these read the
 * unfiltered palette, where neither row is the highlighted one.
 */
describe("D16: host-only commands", () => {
  it("render the host-only badge and are never hidden", () => {
    const h = render();
    h.key("g", { ctrlKey: true });

    const rows = h.all(".fd-palette__row");
    const editorRow = rows.find((r) => r.textContent?.includes("Edit in $EDITOR"));
    expect(editorRow).toBeDefined();
    expect(editorRow?.hidden).toBe(false);
    expect(editorRow?.querySelector(".fd-badge-host")?.textContent).toBe("host only");
  });

  it("Open Worktree in File Manager is present with the badge too", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    const rows = h.all(".fd-palette__row");
    const row = rows.find((r) =>
      r.textContent?.includes("Open Worktree in File Manager"),
    );
    expect(row?.querySelector(".fd-badge-host")).not.toBeNull();
  });

  it("survives a filter that narrows to it — never hidden, only unmatched", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    type(h, "editor");
    const rows = h.all(".fd-palette__row");
    expect(rows.map((r) => r.textContent)).toEqual([
      expect.stringContaining("Edit in $EDITOR"),
    ]);
    expect(rows[0]?.hidden).toBe(false);
  });
});

/**
 * `remote-control-ll5.12`. Every row, tag, badge and refusal below is the
 * host's — `Snapshot::commands` — and the browser has no list of its own to
 * fall back on.
 */
describe("the palette is the host's inventory", () => {
  it("a row the host stops sending disappears, with no code change here", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    type(h, "toggle split");
    expect(h.all(".fd-palette__row")).toHaveLength(1);

    h.key("Escape");
    const full = fixtureSnapshot();
    h.app.store.dispatch({
      type: "snapshot/received",
      snapshot: {
        ...full,
        commands: full.commands.filter((c) => c.id !== "toggle_split_view"),
      },
    });
    h.key("g", { ctrlKey: true });
    type(h, "toggle split");
    expect(h.all(".fd-palette__row")).toHaveLength(0);
    expect(h.text(".fd-palette__count")).toContain("0 of ");
  });

  it("offers nothing at all before a host has said what it implements", () => {
    const h = render();
    h.app.store.dispatch({
      type: "snapshot/received",
      snapshot: { ...fixtureSnapshot(), commands: [] },
    });
    h.key("g", { ctrlKey: true });
    expect(h.all(".fd-palette__row")).toHaveLength(0);
    expect(h.text(".fd-palette__count")).toBe("0 of 0 commands");
  });

  it("shows the host's own refusal sentence on the row, word for word", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    const host = fixtureCommands().find(
      (c) => c.id === "open_worktree_in_file_manager",
    );
    expect(host?.refusal).toBeTruthy();
    const row = rowLabelled(h, "Open Worktree in File Manager");
    expect(row?.title).toBe(host?.refusal);
  });

  it("marks a destructive row the host runs without claiming it is refused", () => {
    /** `abandon_worktree` dispatches since `remote-control-ll5.4`, so the tag
     * beside it is the host's word (`destructive`) and there is no refusal to
     * show — the second step lives on the dialog it opens, not on the row. */
    const h = render();
    h.key("g", { ctrlKey: true });
    const row = rowLabelled(h, "Abandon Worktree");
    expect(row?.textContent).toContain("destructive");
    expect(row?.title).toBe("");
  });

  it("leaves no title on a row the host runs", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    type(h, "open shell");
    expect(h.q(".fd-palette__row").title).toBe("");
  });

  it("D13: the two dialog answers are not palette rows", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    type(h, "dialog");
    const labels = h.all(".fd-palette__row").map((r) => r.textContent ?? "");
    expect(labels.some((l) => l.includes("Confirm Dialog"))).toBe(false);
    expect(labels.some((l) => l.includes("Cancel Dialog"))).toBe(false);
  });
});

/**
 * The bug this task fixes: ten commands open D13's shared dialog on the host,
 * and until now no browser row sent any of them — so a browser could answer a
 * dialog but never start one.
 */
describe("a browser can open every dialog the host offers", () => {
  /** The ten `Route::Palette` rows that open a D13 dialog on the host. Named
   * by id, with the label read off the host's own row — this file words
   * neither. */
  const OPENERS = [
    "open_project",
    "close_project",
    "new_agent_session_tab",
    "rename_agent_session_tab",
    "close_agent_session_tab",
    "close_child_terminal",
    "new_agent",
    "close_agent",
    "set_manual_status",
    "unpair_phone",
  ];

  for (const id of OPENERS) {
    it(`runs ${id} from a palette row`, () => {
      const h = render();
      const label = fixtureCommands().find((c) => c.id === id)?.label ?? "";
      expect(label, `'${id}' is not in the host inventory`).not.toBe("");

      h.key("g", { ctrlKey: true });
      type(h, label);
      const row = rowLabelled(h, label);
      expect(row, `no palette row is labelled '${label}'`).toBeDefined();
      /** No refusal: the host dispatches these through its own palette path,
       * which is what opens the dialog on both surfaces. */
      expect(row?.title).toBe("");

      row?.click();
      expect(h.runCommands.map((c) => c.run.name)).toEqual([id]);
    });
  }

  it("sends them as bare frames — the dialog collects what it needs", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    type(h, "new agent session tab");
    h.q(".fd-palette__row").click();
    expect(h.runCommands[0]?.run).toEqual({ name: "new_agent_session_tab" });
  });
});


describe("outcomes follow the host's Ack, never optimism", () => {
  it("shows nothing until the Ack lands, then the real outcome", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    type(h, "request snapshot");

    const row = h.q(".fd-palette__row");
    expect(row.textContent).toContain("Request Snapshot");
    row.click();

    expect(h.runCommands).toHaveLength(1);
    expect(h.runCommands[0]?.run.name).toBe("request_snapshot");
    // Running it does not itself claim anything happened yet.
    expect(h.maybe(".fd-palette__status")?.hidden ?? true).toBe(true);

    // What `main.ts` does after `sendCommand` returns a seq.
    h.app.store.dispatch({
      type: "palette/dispatched",
      seq: 7,
      label: "Request Snapshot",
    });
    expect(h.q(".fd-palette__status").hidden).toBe(false);
    expect(h.text(".fd-palette__status")).toBe("Request Snapshot: running…");

    // The host's actual Ack, not a guess.
    h.app.store.dispatch({ type: "command/result", seq: 7, outcome: "applied" });
    expect(h.text(".fd-palette__status")).toBe("Request Snapshot: done");
    expect(h.q(".fd-palette__status").getAttribute("data-outcome")).toBe("applied");
  });

  it("a rejected command reports the rejection and detail, not success", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    h.app.store.dispatch({
      type: "palette/dispatched",
      seq: 3,
      label: "Release Seat",
    });
    h.app.store.dispatch({
      type: "command/result",
      seq: 3,
      outcome: "rejected",
      detail: "nothing to release",
    });
    expect(h.text(".fd-palette__status")).toBe(
      "Release Seat: rejected — nothing to release",
    );
  });

  it("D14: an observer's command shows the ReadOnly refusal, not a silent no-op", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    h.app.store.dispatch({
      type: "palette/dispatched",
      seq: 9,
      label: "Select Session: add-tests-api",
    });
    h.app.store.dispatch({
      type: "command/result",
      seq: 9,
      outcome: "read_only",
      detail: "this tab is watching read-only; take over to drive",
    });
    const status = h.q(".fd-palette__status");
    expect(status.getAttribute("data-outcome")).toBe("read_only");
    expect(status.textContent).toBe(
      "Select Session: add-tests-api: read-only — this tab is watching read-only; take over to drive",
    );
  });

  it("ignores a result whose seq nothing is waiting on, rather than guessing", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    h.app.store.dispatch({ type: "command/result", seq: 404, outcome: "applied" });
    expect(h.maybe(".fd-palette__status")?.hidden ?? true).toBe(true);
  });
});

/**
 * `remote-control-ll5.7`. D3: the toggle is a `Command`, exactly like every
 * other palette row — clicking it must never flip `state.layout` itself, and
 * `state.layout` only ever moves once the host's own word arrives
 * (`snapshot/received`/`Delta::Selection`, see `wire/socket.test.ts` and
 * `reducer.test.ts` for that half).
 */
describe("Toggle Split View (remote-control-ll5.7)", () => {
  it("is reachable from the palette and runs as a Command, not a local flip", () => {
    const h = render();
    expect(h.state().layout).toBe("single");
    h.key("g", { ctrlKey: true });
    type(h, "toggle split");

    const row = h.q(".fd-palette__row");
    expect(row.textContent).toContain("Toggle Split View");
    row.click();

    expect(h.runCommands).toHaveLength(1);
    expect(h.runCommands[0]?.run).toEqual({ name: "toggle_split_view" });
    // Running it does not itself claim anything happened, and it certainly
    // does not flip the layout before the host has said anything.
    expect(h.state().layout).toBe("single");
  });

  it("the layout only moves once the host's Selection says so, not on the Ack", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    h.app.store.dispatch({
      type: "palette/dispatched",
      seq: 11,
      label: "Toggle Split View",
    });
    h.app.store.dispatch({ type: "command/result", seq: 11, outcome: "applied" });

    expect(h.text(".fd-palette__status")).toBe("Toggle Split View: done");
    // The Ack alone is not the host's Selection — layout is untouched.
    expect(h.state().layout).toBe("single");

    // What the host's own snapshot (or a live Delta::Selection) does.
    h.app.store.dispatch({ type: "layout/set", layout: "split" });
    expect(h.state().layout).toBe("split");
  });

  it("D14: an observer sees the read-only refusal and the layout never moves", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    h.app.store.dispatch({
      type: "palette/dispatched",
      seq: 12,
      label: "Toggle Split View",
    });
    h.app.store.dispatch({
      type: "command/result",
      seq: 12,
      outcome: "read_only",
      detail: "this tab is watching read-only; take over to drive",
    });

    expect(h.text(".fd-palette__status")).toBe(
      "Toggle Split View: read-only — this tab is watching read-only; take over to drive",
    );
    expect(h.state().layout).toBe("single");
  });
});

describe("keyboard navigation", () => {
  it("↑↓ move the highlighted row within a column", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    const selected = () => h.q('.fd-palette__row[data-selected="true"]').textContent;
    const first = selected();

    h.key("ArrowDown");
    expect(selected()).not.toBe(first);

    h.key("ArrowUp");
    expect(selected()).toBe(first);
  });

  it("Tab moves the highlight to the other column", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    expect(h.state().palette?.column).toBe(0);
    h.key("Tab");
    expect(h.state().palette?.column).toBe(1);
    const columnOneSelected = h.q(
      '.fd-palette__column[data-column="1"] .fd-palette__row[data-selected="true"]',
    );
    expect(columnOneSelected).toBeTruthy();
  });

  it("Enter runs the highlighted row", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    h.key("Enter");
    expect(h.runCommands).toHaveLength(1);
  });

  it("Esc closes the palette instead of typing a character", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    type(h, "wor");
    h.key("Escape");
    expect(h.state().palette).toBeNull();
  });
});
