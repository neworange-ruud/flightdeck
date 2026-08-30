/**
 * @vitest-environment jsdom
 *
 * `remote-control-1p22` — artboard 1f, driven the way production drives it.
 *
 * **Every test in this file delivers a frame or reads one that went out. None
 * of them dispatches an action.** That is the whole point of the file: the bug
 * it was written for is one the old `ui/configManager.test.ts` could not have
 * caught, because it dispatched `config/open` — supplying the exact thing
 * production was missing — and then asserted that a renderer full of invented
 * per-layer values drew them. Its keys did not even match the host's, and `s`
 * sent a command name (`save_config`) the host has never had.
 *
 * So the store here is the *real* app's store, `openSession` is wired to it
 * through a fake `WebSocket`, and every assertion is either against the DOM the
 * user would be looking at or against the JSON that really left the socket.
 * Same door as `wire/wiredScreen.test.ts` (§6.5 R21), for the same reason.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createApp } from "../ui/app";
import type { App } from "../ui/app";
import { openSession } from "./socket";
import type { SessionSocket } from "./socket";
import { PROTOCOL_VERSION } from "./frames";

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

/** One host snapshot, carrying the palette row this whole flow starts at. The
 * inventory is the host's (R7), so the row has to arrive before the browser
 * can offer it. */
function snapshotFrame(): string {
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
            git: {
              branch: "flightdeck/fix-login",
              added: 0,
              modified: 0,
              removed: 0,
              ahead: 0,
              behind: 0,
              drift: 0,
              has_upstream: true,
              files_changed: 0,
              collected: true,
            },
            terminals: [
              {
                terminal_id: TERMINAL,
                session_id: "tab-1",
                role: "primary",
                title: "agent",
                geometry: { cols: 120, rows: 34 },
                byte_len: 0,
                replay_from: 0,
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
    commands: [
      {
        id: "open_configuration",
        label: "Open Configuration",
        group: "Configuration",
        run: { name: "open_configuration" },
      },
    ],
  });
}

/**
 * A `configuration` frame as `web_config_view` builds one.
 *
 * The keys are the host's real ones (`notifications.on_finish`, `update.check`)
 * and the two numeric `[web]` fields the manager deliberately does not curate
 * are absent — because the host's own field list is what this frame is a
 * serialisation of.
 */
function configFrame(over: {
  readonly seq: number;
  readonly finishSetHere?: boolean;
}): string {
  const project = [
    {
      key: "notifications.enabled",
      label: "OS notifications",
      kind: "bool",
      value: false,
      origin: "global",
      inherited: true,
      inherited_origin: "default",
    },
    {
      key: "notifications.on_finish",
      label: "Notify when finished",
      kind: "bool",
      value: over.finishSetHere === true,
      origin: over.finishSetHere === true ? "set_here" : "default",
      inherited: false,
      inherited_origin: "default",
    },
    {
      key: "ui.mode_border",
      label: "Mode border",
      kind: "choice",
      value: "off",
      choices: ["off", "dim", "normal", "bright"],
      origin: "default",
      inherited: "off",
      inherited_origin: "default",
    },
    {
      key: "web.bind",
      label: "Web interface bind address",
      kind: "text",
      value: "127.0.0.1",
      origin: "default",
      inherited: "127.0.0.1",
      inherited_origin: "default",
    },
  ];
  const global = [
    {
      key: "notifications.enabled",
      label: "OS notifications",
      kind: "bool",
      value: false,
      origin: "set_here",
      inherited: true,
      inherited_origin: "default",
    },
  ];
  return JSON.stringify({
    type: "configuration",
    seq: over.seq,
    project_name: "flightdeck",
    global_path: "/home/u/.flightdeck/config.toml",
    project_path: "/repo/.flightdeck/config.toml",
    project_rows: project,
    global_rows: global,
  });
}

function ackFrame(seq: number, detail?: string): string {
  return JSON.stringify({
    type: "ack",
    seq,
    outcome: "applied",
    ...(detail === undefined ? {} : { detail }),
  });
}

interface SentCommand {
  readonly seq: number;
  readonly name: string;
  readonly args?: {
    readonly changes: readonly {
      readonly scope: string;
      readonly key: string;
      readonly value?: unknown;
    }[];
  };
}

interface Harness {
  readonly app: App;
  deliver(frame: string): void;
  /** Every `command` frame that really left the socket, in order. */
  commands(): readonly SentCommand[];
  key(key: string, init?: KeyboardEventInit): void;
  type(text: string): void;
  text(selector: string): string;
  all(selector: string): readonly HTMLElement[];
  maybe(selector: string): HTMLElement | null;
  /** One config row's four cells, by the host's key. */
  row(key: string): { value: string; label: string; origin: string };
}

const open: { close(): void }[] = [];

function harness(): Harness {
  const sent: string[] = [];
  let live: FakeSocket | null = null;
  let session: SessionSocket | null = null;
  /**
   * The two seams `main.ts` wires, wired the same way here — because the frame
   * a palette row produces, and the frame `s` produces, are exactly what these
   * tests are about. Everything else about the app is untouched.
   */
  const app = createApp({
    mount: () => undefined,
    onRunCommand: (command) => {
      const seq = session?.sendCommand(command.run.name, command.run.args);
      if (seq !== undefined) {
        app.store.dispatch({ type: "palette/dispatched", seq, label: command.label });
      }
    },
    onSaveConfig: (request) => {
      const seq = session?.sendCommand("open_configuration", request);
      if (seq !== undefined) {
        app.store.dispatch({ type: "config/dispatched", seq });
      }
    },
  });
  document.body.append(app.el);
  session = openSession({
    store: app.store,
    url: "ws://test/ws",
    socketFactory: () => {
      const ws: FakeSocket = {
        onopen: null,
        onmessage: null,
        onclose: null,
        onerror: null,
        readyState: 1,
        send: (data) => sent.push(data),
        close: () => undefined,
      };
      live = ws;
      return ws as unknown as WebSocket;
    },
  });
  open.push(session);
  const socket = (): FakeSocket => {
    if (live === null) {
      throw new Error("no socket was opened");
    }
    return live;
  };
  socket().onopen?.();

  const find = (selector: string): HTMLElement | null =>
    document.querySelector<HTMLElement>(selector);

  return {
    app,
    deliver: (frame) => socket().onmessage?.({ data: frame }),
    commands: () =>
      sent
        .map((raw) => JSON.parse(raw) as { type: string })
        .filter((frame): frame is SentCommand & { type: string } =>
          frame.type === "command",
        ),
    key: (key, init = {}) => {
      app.el.dispatchEvent(
        new KeyboardEvent("keydown", { key, bubbles: true, ...init }),
      );
    },
    type: (text) => {
      for (const char of text) {
        app.el.dispatchEvent(
          new KeyboardEvent("keydown", { key: char, bubbles: true }),
        );
      }
    },
    text: (selector) => find(selector)?.textContent ?? "",
    all: (selector) => [
      ...document.querySelectorAll<HTMLElement>(selector),
    ],
    maybe: find,
    row: (key) => {
      const el = find(`.fd-config__row[data-key="${key}"]`);
      if (el === null) {
        throw new Error(`no config row for ${key}`);
      }
      return {
        value: el.querySelector(".fd-config__value")?.textContent ?? "",
        label: el.querySelector(".fd-config__label")?.textContent ?? "",
        origin: el.querySelector(".fd-config__origin")?.textContent ?? "",
      };
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
 * Opens the palette and runs the host's "Open Configuration" row, exactly as a
 * user does: `Ctrl-g`, type to filter, `Tab` into 1d's right-hand column (where
 * the `Configuration` group lives), `Enter`.
 *
 * The row is the *host's* — it arrives on `Snapshot::commands` — so this
 * whole flow stops working the moment a host stops offering the name, which is
 * the property R7 exists to give.
 */
function runOpenConfiguration(h: Harness): void {
  h.key("g", { ctrlKey: true });
  h.type("Open Config");
  h.key("Tab");
  h.key("Enter");
}

describe("1f is opened by a frame going out and a frame coming back", () => {
  it("sends open_configuration and shows nothing until the host answers", () => {
    const h = harness();
    h.deliver(snapshotFrame());

    runOpenConfiguration(h);

    const sent = h.commands();
    expect(sent).toHaveLength(1);
    expect(sent[0]?.name).toBe("open_configuration");
    /** A plain open carries no edits at all — the host reads, it does not
     * write. */
    expect(sent[0]?.args).toBeUndefined();

    /**
     * The defect, as an assertion: before the host answers there is no panel,
     * because there is nothing honest to put in one. The old build opened a
     * full manager here, populated from a constant.
     */
    expect(h.app.store.getState().config).toBeNull();
    expect(h.maybe(".fd-config")?.hidden).toBe(true);
    expect(h.all(".fd-config__row")).toHaveLength(0);

    h.deliver(configFrame({ seq: sent[0]?.seq ?? 0 }));
    expect(h.maybe(".fd-config")?.hidden).toBe(false);
  });

  it("draws the host's keys, values and origin tags and nothing else", () => {
    const h = harness();
    h.deliver(snapshotFrame());
    runOpenConfiguration(h);
    h.deliver(configFrame({ seq: 1 }));

    /** The host's real keys. `notifications.on_finished` and
     * `updates.check_for_updates` were the invented ones, and nothing can
     * produce them now because the rows are the frame's. */
    const keys = h
      .all(".fd-config__row")
      .map((row) => row.getAttribute("data-key"));
    expect(keys).toEqual([
      "notifications.enabled",
      "notifications.on_finish",
      "ui.mode_border",
      "web.bind",
    ]);

    /** 1f's four cells, per row, from the frame. */
    expect(h.row("notifications.enabled")).toEqual({
      value: "[ ]",
      label: "OS notifications",
      origin: "(global)",
    });
    expect(h.row("ui.mode_border").value).toBe("‹off›");
    expect(h.row("web.bind").value).toBe("127.0.0.1");

    /** And the header, likewise: the project's name and the file being edited
     * are the host's, not a guess from the selection. */
    expect(h.text(".fd-config__scopes")).toContain("Project (flightdeck)");
    expect(h.text(".fd-config__path")).toBe("/repo/.flightdeck/config.toml");
  });

  it("switches scope to the other list the host sent, with its own file", () => {
    const h = harness();
    h.deliver(snapshotFrame());
    runOpenConfiguration(h);
    h.deliver(configFrame({ seq: 1 }));

    h.key("Tab");

    /** The same setting, one layer down: what reads `(global)` from the
     * project's point of view is `(set here)` from the global file's. Both
     * answers came in one frame, so the two cannot disagree — and switching
     * cost no round trip. */
    expect(h.row("notifications.enabled").origin).toBe("(set here)");
    expect(h.text(".fd-config__path")).toBe("/home/u/.flightdeck/config.toml");
    expect(h.commands()).toHaveLength(1);
  });
});

describe("an edit is staged, then travels as one frame", () => {
  it("Space toggles the selected row and s sends what was staged", () => {
    const h = harness();
    h.deliver(snapshotFrame());
    runOpenConfiguration(h);
    h.deliver(configFrame({ seq: 1 }));

    /** 1f's footer has promised `Space toggle / edit` since turn 1; before
     * `remote-control-1p22` it was bound to nothing. */
    h.key("ArrowDown");
    h.key(" ");

    expect(h.row("notifications.on_finish")).toEqual({
      value: "[x]",
      label: "Notify when finished",
      origin: "(set here)",
    });
    expect(h.maybe(".fd-config__unsaved")?.hidden).toBe(false);
    /** Staging is local: nothing has been sent yet. */
    expect(h.commands()).toHaveLength(1);

    h.key("s");

    const sent = h.commands();
    expect(sent).toHaveLength(2);
    expect(sent[1]?.name).toBe("open_configuration");
    expect(sent[1]?.args?.changes).toEqual([
      { scope: "project", key: "notifications.on_finish", value: true },
    ]);
  });

  it("cycles a choice through the host's own options, never a local list", () => {
    const h = harness();
    h.deliver(snapshotFrame());
    runOpenConfiguration(h);
    h.deliver(configFrame({ seq: 1 }));

    h.key("ArrowDown");
    h.key("ArrowDown");
    h.key(" ");
    expect(h.row("ui.mode_border").value).toBe("‹dim›");
    h.key(" ");
    expect(h.row("ui.mode_border").value).toBe("‹normal›");

    h.key("s");
    expect(h.commands()[1]?.args?.changes).toEqual([
      { scope: "project", key: "ui.mode_border", value: "normal" },
    ]);
  });

  it("c stages a clear that shows the value the host said it would inherit", () => {
    const h = harness();
    h.deliver(snapshotFrame());
    runOpenConfiguration(h);
    /** This build's `notifications.on_finish` is an explicit project
     * override. */
    h.deliver(configFrame({ seq: 1, finishSetHere: true }));
    expect(h.row("notifications.on_finish").origin).toBe("(set here)");

    h.key("ArrowDown");
    h.key("c");

    /** The row now reads the host's own `inherited` / `inherited_origin` — no
     * layer was walked in this browser to produce it. */
    expect(h.row("notifications.on_finish")).toEqual({
      value: "[ ]",
      label: "Notify when finished",
      origin: "(default)",
    });

    h.key("s");
    /** A clear is an absent value, never `false`: a toggle's `false` is a
     * legal explicit override. */
    expect(h.commands()[1]?.args?.changes).toEqual([
      { scope: "project", key: "notifications.on_finish" },
    ]);
  });

  it("edits a text field inline and stages what was typed", () => {
    const h = harness();
    h.deliver(snapshotFrame());
    runOpenConfiguration(h);
    h.deliver(configFrame({ seq: 1 }));

    for (let i = 0; i < 3; i += 1) {
      h.key("ArrowDown");
    }
    /** `Space` on a text row opens the editor rather than toggling — which of
     * the two happens is decided by the host's `kind`. */
    h.key(" ");
    expect(h.row("web.bind").value).toBe("127.0.0.1█");

    for (let i = 0; i < "127.0.0.1".length; i += 1) {
      h.key("Backspace");
    }
    h.type("0.0.0.0");
    h.key("Enter");

    expect(h.row("web.bind").value).toBe("0.0.0.0");
    expect(h.row("web.bind").origin).toBe("(set here)");
    /** D5's caution, computed from the host's value and worded by the design —
     * the same division R2 made for the git bar's lifecycle note. */
    expect(h.text(".fd-config__note--inline")).toContain("routable");

    h.key("s");
    expect(h.commands()[1]?.args?.changes).toEqual([
      { scope: "project", key: "web.bind", value: "0.0.0.0" },
    ]);
  });

  it("Esc during an inline edit discards it and leaves the panel open", () => {
    const h = harness();
    h.deliver(snapshotFrame());
    runOpenConfiguration(h);
    h.deliver(configFrame({ seq: 1 }));

    for (let i = 0; i < 3; i += 1) {
      h.key("ArrowDown");
    }
    h.key(" ");
    h.type("x");
    h.key("Escape");

    expect(h.row("web.bind").value).toBe("127.0.0.1");
    expect(h.maybe(".fd-config")?.hidden).toBe(false);
    expect(h.maybe(".fd-config__unsaved")?.hidden).toBe(true);
  });

  it("carries both scopes in one save, as the desktop's s does", () => {
    const h = harness();
    h.deliver(snapshotFrame());
    runOpenConfiguration(h);
    h.deliver(configFrame({ seq: 1 }));

    h.key(" ");
    h.key("Tab");
    h.key(" ");
    h.key("s");

    expect(h.commands()[1]?.args?.changes).toEqual([
      { scope: "global", key: "notifications.enabled", value: true },
      { scope: "project", key: "notifications.enabled", value: true },
    ]);
  });

  it("sends nothing when nothing is staged", () => {
    const h = harness();
    h.deliver(snapshotFrame());
    runOpenConfiguration(h);
    h.deliver(configFrame({ seq: 1 }));

    h.key("s");

    expect(h.commands()).toHaveLength(1);
  });
});

describe("the save's answer is the host's, never the browser's optimism", () => {
  it("keeps the edits staged until a real applied ack, then repaints from the frame", () => {
    const h = harness();
    h.deliver(snapshotFrame());
    runOpenConfiguration(h);
    h.deliver(configFrame({ seq: 1 }));

    h.key("ArrowDown");
    h.key(" ");
    h.key("s");
    const saveSeq = h.commands()[1]?.seq ?? 0;

    /** While the frame is in flight the panel says so, and the edit is still
     * staged: nothing has been written yet. */
    expect(h.text(".fd-config__status")).toBe("Saving…");
    expect(h.maybe(".fd-config__unsaved")?.hidden).toBe(false);

    h.deliver(ackFrame(saveSeq, "Saved."));
    h.deliver(configFrame({ seq: saveSeq, finishSetHere: true }));

    /** The host's own word, and the host's own rows. */
    expect(h.text(".fd-config__status")).toBe("Saved.");
    expect(h.maybe(".fd-config__unsaved")?.hidden).toBe(true);
    expect(h.row("notifications.on_finish").origin).toBe("(set here)");
  });

  it("asks for a fresh snapshot, because a saved setting can change the layout", () => {
    /**
     * §6.5 R24. `[ui] agent_tab_position` is a config key the *browser lays out
     * from*, and it reaches the browser on `Snapshot::sidebar_position`. So a
     * save that lands has to be followed by a snapshot, or the desktop mirrors
     * its sidebar and this tab does not until it is reloaded.
     */
    const h = harness();
    h.deliver(snapshotFrame());
    runOpenConfiguration(h);
    h.deliver(configFrame({ seq: 1 }));

    /** Coalesced: it is a timer, exactly like every other resync this socket
     * asks for. */
    expect(h.commands().map((c) => c.name)).not.toContain("request_snapshot");
    vi.advanceTimersByTime(200);
    expect(h.commands().map((c) => c.name)).toContain("request_snapshot");
  });

  it("leaves a refused save staged, in the host's words", () => {
    const h = harness();
    h.deliver(snapshotFrame());
    runOpenConfiguration(h);
    h.deliver(configFrame({ seq: 1 }));

    h.key("ArrowDown");
    h.key(" ");
    h.key("s");
    const saveSeq = h.commands()[1]?.seq ?? 0;

    h.deliver(
      JSON.stringify({
        type: "ack",
        seq: saveSeq,
        outcome: "rejected",
        detail: "`ui.mode_border` is one of: off, dim, normal, bright.",
      }),
    );

    expect(h.text(".fd-config__status")).toContain(
      "off, dim, normal, bright",
    );
    /** Still staged, so the user can see exactly what did not land and press
     * `s` again — never cleared by the keypress itself. */
    expect(h.maybe(".fd-config__unsaved")?.hidden).toBe(false);
    expect(h.row("notifications.on_finish").origin).toBe("(set here)");
  });
});
