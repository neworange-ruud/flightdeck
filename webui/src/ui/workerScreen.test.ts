/**
 * @vitest-environment jsdom
 *
 * A worker's window, rendered (`remote-control-vjtb`): the shell's tab ahead
 * of the projects, the card a fresh worker opens on, the picker, the taken-over
 * panel, the status bar's worker line and the access screen's worker words —
 * and, against the TUI fixture, that none of it appears on any other host.
 *
 * xterm.js is not involved, as in `mainScreen.test.ts`: `mount` is injected
 * and records what it was asked to draw, which is exactly the part W5 changes.
 */
import { beforeEach, describe, expect, it } from "vitest";
import type { StripAction } from "../state/connection";
import { fixtureSnapshot } from "../state/fixture";
import type { AppAction, TerminalGeometry } from "../state/types";
import { snapshotFromWire } from "../wire/adapt";
import type { WireSnapshot } from "../wire/frames";
import { workerProject, workerSnapshotWire, workerWire } from "../wire/workerFixture";
import { createApp } from "./app";
import type { App } from "./app";

interface Harness {
  readonly app: App;
  readonly mounts: { geometry: TerminalGeometry; terminalId: string }[];
  readonly resizes: { geometry: TerminalGeometry; terminalId: string }[];
  readonly disposes: string[];
  readonly dispatched: AppAction[];
  readonly strip: StripAction[];
  q: (selector: string) => HTMLElement;
  maybe: (selector: string) => HTMLElement | null;
  text: (selector: string) => string;
}

function render(snapshot: WireSnapshot | "tui" = workerSnapshotWire()): Harness {
  const mounts: Harness["mounts"] = [];
  const resizes: Harness["resizes"] = [];
  const disposes: string[] = [];
  const dispatched: AppAction[] = [];
  const strip: StripAction[] = [];
  const app = createApp({
    mount: (container, geometry, terminalId) => {
      mounts.push({ geometry, terminalId });
      container.append(`[${terminalId}]`);
      return {
        dispose: () => disposes.push(terminalId),
        resize: (next) => resizes.push({ geometry: next, terminalId }),
      };
    },
    onDispatch: (action) => dispatched.push(action),
    onStripAction: (action) => strip.push(action),
  });
  document.body.append(app.el);
  app.store.dispatch({
    type: "snapshot/received",
    snapshot: snapshot === "tui" ? fixtureSnapshot() : snapshotFromWire(snapshot),
  });
  app.store.dispatch({ type: "connection/changed", status: "connected" });
  const maybe = (selector: string): HTMLElement | null =>
    app.el.querySelector<HTMLElement>(selector);
  const q = (selector: string): HTMLElement => {
    const found = maybe(selector);
    if (found === null) {
      throw new Error(`no element matched ${selector}`);
    }
    return found;
  };
  return {
    app,
    mounts,
    resizes,
    disposes,
    dispatched,
    strip,
    q,
    maybe,
    text: (selector) => q(selector).textContent ?? "",
  };
}

function visible(element: HTMLElement | null): boolean {
  return element !== null && !element.hidden;
}

beforeEach(() => {
  document.body.replaceChildren();
});

describe("on a TUI host nothing changes", () => {
  it("draws no worker tab, card, picker or panel, and keeps + project host-only", () => {
    const h = render("tui");
    expect(h.maybe(".fd-project--shell")).toBeNull();
    expect(h.app.el.hasAttribute("data-host")).toBe(false);
    expect(visible(h.maybe(".fd-workercard"))).toBe(false);
    expect(visible(h.maybe(".fd-picker"))).toBe(false);
    expect(visible(h.maybe(".fd-takenover"))).toBe(false);
    expect(h.maybe(".fd-statusbar__worker")).toBeNull();
    expect(h.text(".fd-action--project")).toContain("host only");
    expect(h.text(".fd-geometry")).toContain("host owns geometry");
  });
});

describe("a fresh worker", () => {
  it("puts its shell's tab first and opens on the No projects yet card", () => {
    const h = render();
    expect(h.app.el.getAttribute("data-host")).toBe("worker");
    const tabs = [...h.app.el.querySelectorAll<HTMLElement>('.fd-projects [role="tab"]')];
    expect(tabs[0]?.textContent).toContain("Worker shell");
    expect(h.q(".fd-workercard").getAttribute("data-kind")).toBe("empty");
    expect(h.text(".fd-workercard")).toContain("This worker has no projects yet.");
    expect(h.app.el.getAttribute("data-worker-card")).toBe("empty");
    /** Nothing to draw: no terminal was mounted for a selection that is not. */
    expect(h.mounts).toEqual([]);
  });

  it("Open Project, from the card or the project row, asks for the picker", () => {
    const h = render();
    h.q('.fd-workercard [data-action="open-project"]').click();
    expect(h.dispatched).toContainEqual({ type: "picker/request", intent: { kind: "open" } });
    expect(visible(h.q(".fd-picker"))).toBe(true);
    expect(h.text(".fd-action--project")).toContain("Open Project");
  });
});

describe("the worker shell's tab (W15)", () => {
  it("shows the shell full width, at its own grid, and moves nothing on the host", () => {
    const h = render(workerSnapshotWire({ projects: [workerProject()] }));
    expect(h.mounts.map((m) => m.terminalId)).toEqual(["t-agent"]);
    expect(h.mounts[0]?.geometry).toEqual({ cols: 132, rows: 40 });

    h.q(".fd-project__select--shell").click();
    expect(h.app.el.getAttribute("data-worker-shell")).toBe("true");
    expect(h.mounts.at(-1)).toEqual({ terminalId: "worker:shell", geometry: { cols: 80, rows: 24 } });
    expect(h.dispatched.filter((a) => a.type.startsWith("selection/"))).toEqual([]);

    /** A project tab chosen again leaves the shell. */
    h.q('.fd-project [role="tab"][aria-selected="false"]').click();
    expect(h.app.el.getAttribute("data-worker-shell")).toBe("false");
  });

  it("resizes the mounted terminal in place when its grid comes back (W5)", () => {
    const h = render();
    h.q(".fd-project__select--shell").click();
    const mounted = h.mounts.length;
    h.app.store.dispatch({
      type: "worker/shell",
      shell: { terminalId: "worker:shell", title: "Worker shell", alive: true, exitCode: null },
      geometry: { cols: 150, rows: 45 },
    });
    expect(h.mounts).toHaveLength(mounted);
    expect(h.resizes).toEqual([
      { terminalId: "worker:shell", geometry: { cols: 150, rows: 45 } },
    ]);
    expect(h.text(".fd-geometry")).toBe("150×45 · sized to this window");
  });

  it("offers Restart shell once the shell has exited", () => {
    const h = render();
    h.q(".fd-project__select--shell").click();
    h.app.store.dispatch({
      type: "worker/shell",
      shell: { terminalId: "worker:shell", title: "Worker shell", alive: false, exitCode: 127 },
      geometry: { cols: 80, rows: 24 },
    });
    expect(h.q(".fd-workercard").getAttribute("data-kind")).toBe("shell_exited");
    expect(h.text(".fd-workercard")).toContain("The worker shell exited (127).");
    h.q('[data-action="restart-shell"]').click();
    expect(h.dispatched).toContainEqual({ type: "workerShell/restart" });
  });

  it("says the shell is not running when the worker has none", () => {
    const h = render(workerSnapshotWire({ worker: workerWire({ shell: null }) }));
    h.q(".fd-project__select--shell").click();
    expect(h.q(".fd-workercard").getAttribute("data-kind")).toBe("shell_missing");
    expect(h.text(".fd-workercard")).toContain("The worker shell is not running.");
  });
});

describe("the project picker (W9)", () => {
  function opened(): Harness {
    const h = render();
    h.app.store.dispatch({ type: "picker/request", intent: { kind: "open" } });
    h.app.store.dispatch({ type: "picker/sent", sent: { kind: "scan" }, seq: 1 });
    h.app.store.dispatch({ type: "picker/sent", sent: { kind: "browse" }, seq: 2 });
    h.app.store.dispatch({
      type: "picker/scan",
      scan: {
        seq: 1,
        root: "/home/me/flightdeck",
        rootExists: true,
        repos: [{ name: "beta", path: "/home/me/flightdeck/beta", isOpenProject: false }],
        refusal: null,
      },
    });
    h.app.store.dispatch({
      type: "picker/listing",
      listing: {
        seq: 2,
        path: "/home/me",
        parent: null,
        home: "/home/me",
        entries: [
          { name: "code", path: "/home/me/code", isGitRepo: false, isOpenProject: false },
          { name: "alpha", path: "/home/me/alpha", isGitRepo: true, isOpenProject: true },
        ],
        files: [],
        truncated: false,
      },
    });
    return h;
  }

  it("lists the scan and the folder, and says which repositories are open", () => {
    const h = opened();
    expect(h.text(".fd-picker [data-section='scan']")).toContain("PROJECTS IN ~/flightdeck");
    expect(h.maybe('[data-list="scan"] [data-name="beta"] [data-action="open"]')).not.toBeNull();
    expect(h.maybe('[data-list="browse"] [data-name="code"] [data-action="open"]')).toBeNull();
    expect(h.text('[data-list="browse"] [data-name="alpha"]')).toContain("open");
    expect(h.q('[data-action="up"]').hasAttribute("disabled")).toBe(true);
  });

  it("every click is a request; nothing is sent from here", () => {
    const h = opened();
    h.q('[data-list="scan"] [data-name="beta"] [data-action="open"]').click();
    h.q('[data-list="browse"] [data-name="code"] [data-action="enter"]').click();
    h.q('[data-action="toggle-hidden"]').click();
    const requests = h.dispatched
      .filter((a) => a.type === "picker/request")
      .map((a) => (a.type === "picker/request" ? a.intent : null));
    expect(requests).toEqual([
      { kind: "open" },
      { kind: "open_path", path: "/home/me/flightdeck/beta" },
      { kind: "browse", path: "/home/me/code" },
      { kind: "toggle_hidden" },
    ]);
  });

  it("keeps what is typed across renders, and Enter goes there", () => {
    const h = opened();
    const path = h.q(".fd-picker__input[name='path']") as HTMLInputElement;
    expect(path.value).toBe("/home/me");
    path.value = "/home/me/co";
    path.dispatchEvent(new Event("input"));
    h.app.store.dispatch({ type: "notice/set", notice: "re-render" });
    expect(path.value).toBe("/home/me/co");
    path.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(h.dispatched).toContainEqual({
      type: "picker/request",
      intent: { kind: "browse", path: "/home/me/co" },
    });
  });

  it("keys typed into it are not shortcuts, and Esc closes it", () => {
    const h = opened();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "a" }));
    expect(h.app.store.getState().feedOpen).toBe(false);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(visible(h.q(".fd-picker"))).toBe(false);
  });

  it("shows the worker's refusal on its error line", () => {
    const h = opened();
    h.app.store.dispatch({ type: "picker/error", error: CLONE_ERROR });
    expect(h.text(".fd-picker__error")).toBe(CLONE_ERROR);
  });
});

const CLONE_ERROR = "Type the URL of the repository to clone.";

describe("taken over (W17)", () => {
  it("names who took it and offers only Take it back", () => {
    const h = render();
    h.app.store.dispatch({ type: "connection/takenOver", by: "FlightDeck Desktop (laptop)" });
    expect(visible(h.q(".fd-takenover"))).toBe(true);
    expect(h.text(".fd-takenover")).toContain("Taken over by FlightDeck Desktop (laptop)");
    expect(h.text(".fd-statusbar .fd-conn")).toContain("taken over");

    h.q('.fd-takenover [data-action="reclaim"]').click();
    expect(h.strip.map((a) => a.kind)).toEqual(["reclaim"]);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter" }));
    expect(h.strip.map((a) => a.kind)).toEqual(["reclaim", "reclaim"]);
  });
});

describe("the status bar's worker line", () => {
  it("is the health line, and a notice when there is one", () => {
    const h = render();
    expect(h.text(".fd-statusbar__health")).toBe("load 0.42 · 18 GB free · 3 agents");
    h.app.store.dispatch({
      type: "worker/message",
      message: { id: 1, atMs: 1, level: "warning", text: "disk low" },
    });
    expect(h.text(".fd-statusbar__notice")).toBe("Warning: disk low");
    expect(h.maybe(".fd-statusbar__health")).toBeNull();
  });

  it("never moves the connection strip", () => {
    const h = render();
    h.app.store.dispatch({ type: "notifications/permission", permission: "default" });
    expect(
      h.q(".fd-statusbar .fd-spacer").nextElementSibling?.classList.contains("fd-conn"),
    ).toBe(true);
  });

  it("offers Enable notifications only when the browser can still ask", () => {
    const h = render();
    expect(h.maybe(".fd-statusbar__notify")).toBeNull();
    h.app.store.dispatch({ type: "notifications/permission", permission: "default" });
    expect(h.text(".fd-statusbar__notify")).toBe("Enable notifications");
    h.app.store.dispatch({ type: "notifications/permission", permission: "denied" });
    expect(h.maybe(".fd-statusbar__notify")).toBeNull();
  });
});

describe("a worker's words in front of the reader", () => {
  it("the kept messages are one panel, the count as its subtitle", () => {
    const h = render();
    h.app.store.dispatch({
      type: "worker/away",
      messages: [
        { id: 1, atMs: 1, level: "info", text: "update available" },
        { id: 2, atMs: 2, level: "error", text: "clone failed" },
      ],
    });
    expect(h.text(".fd-info__title")).toBe("While you were away");
    expect(h.text(".fd-info__subtitle")).toBe("2 messages while you were away");
    const lines = [...h.app.el.querySelectorAll(".fd-info__message")].map((p) => p.textContent);
    expect(lines).toEqual(["update available", "Error: clone failed"]);
  });
});

describe("the palette on a worker", () => {
  it("runs Open Project as the picker rather than sending it", () => {
    const h = render(
      workerSnapshotWire({
        commands: [
          {
            id: "open_project",
            label: "Open Project",
            group: "Projects",
            run: { name: "open_project" },
            refusal: "Use the worker's project picker (browse, scan, clone).",
          },
          {
            id: "quit",
            label: "Quit",
            group: "Global",
            run: { name: "quit" },
            refusal: "A worker is stopped from its own box.",
          },
        ],
      }),
    );
    h.app.store.dispatch({ type: "palette/open" });
    const palette = h.text(".fd-palette");
    expect(palette).toContain("Open Project");
    expect(palette).not.toContain("Quit");
    const open = [...h.app.el.querySelectorAll<HTMLElement>(".fd-palette__row")].find((row) =>
      (row.textContent ?? "").includes("Open Project"),
    );
    open?.click();
    expect(h.dispatched.some((a) => a.type === "palette/dispatched")).toBe(false);
    expect(h.app.store.getState().picker).not.toBeNull();
    expect(h.app.store.getState().palette).toBeNull();
  });
});

describe("the access screen on a worker", () => {
  it("sends the reader to flightdeck worker pair", () => {
    const h = render();
    h.app.store.dispatch({
      type: "access/required",
      screen: "code_entry",
      attemptsRemaining: null,
      lockoutSeconds: null,
      lockoutLengthSeconds: null,
      codeTtlSeconds: null,
      hostKind: "worker",
    });
    expect(h.text(".fd-access")).toContain("flightdeck worker pair");
    expect(h.text(".fd-access")).not.toContain("Ctrl-g");
  });
});
