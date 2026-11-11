import { describe, expect, it } from "vitest";
import { snapshotFromWire } from "../wire/adapt";
import { workerProject, workerSnapshotWire } from "../wire/workerFixture";
import { accessCopy } from "./access";
import { paletteInventory } from "./commands";
import { connectionStrip } from "./connection";
import { fixtureSnapshot } from "./fixture";
import type { AccessState, HostCommand } from "./model";
import { reduce } from "./reducer";
import { createInitialState, isTerminalConnection } from "./types";
import type { AppAction, AppState } from "./types";

/**
 * The reducer's half of a worker's window (`remote-control-vjtb`): what each
 * worker action does, and — just as much — that none of it reaches a TUI or
 * Desktop host.
 */

function apply(state: AppState, ...actions: AppAction[]): AppState {
  return actions.reduce(reduce, state);
}

function worker(projects = [workerProject()]): AppState {
  return apply(createInitialState(), {
    type: "snapshot/received",
    snapshot: snapshotFromWire(workerSnapshotWire({ projects })),
  });
}

function tui(): AppState {
  return apply(createInitialState(), {
    type: "snapshot/received",
    snapshot: fixtureSnapshot(),
  });
}

describe("a worker's snapshot", () => {
  it("carries the host kind, the worker and every terminal's own grid", () => {
    const state = worker();
    expect(state.hostKind).toBe("worker");
    expect(state.worker?.home).toBe("/home/me");
    expect(state.worker?.shell?.terminalId).toBe("worker:shell");
    expect(state.terminalGeometry).toEqual({
      "t-agent": { cols: 132, rows: 40 },
      "worker:shell": { cols: 80, rows: 24 },
    });
  });

  it("leaves a TUI host with none of it", () => {
    const state = tui();
    expect(state.hostKind).toBeNull();
    expect(state.worker).toBeNull();
    expect(state.workerShellTab).toBe(false);
    expect(state.picker).toBeNull();
  });
});

describe("the worker shell's tab", () => {
  it("is chosen locally and left by choosing a project, session or terminal", () => {
    const shell = apply(worker(), { type: "workerShell/show", show: true });
    expect(shell.workerShellTab).toBe(true);
    expect(apply(shell, { type: "selection/project", projectId: "p-alpha" }).workerShellTab).toBe(false);
    expect(apply(shell, { type: "selection/session", sessionId: "s-1" }).workerShellTab).toBe(false);
    expect(apply(shell, { type: "selection/terminal", terminalId: "t-agent" }).workerShellTab).toBe(false);
  });

  it("survives the worker's next snapshot, since the host knows nothing of it", () => {
    const shell = apply(worker(), { type: "workerShell/show", show: true });
    const again = apply(shell, {
      type: "snapshot/received",
      snapshot: snapshotFromWire(workerSnapshotWire({ projects: [workerProject()] })),
    });
    expect(again.workerShellTab).toBe(true);
  });

  it("does not exist on a TUI host", () => {
    const state = tui();
    expect(apply(state, { type: "workerShell/show", show: true })).toBe(state);
  });

  it("follows the shell's own delta, grid included", () => {
    const state = apply(worker(), {
      type: "worker/shell",
      shell: { terminalId: "worker:shell", title: "sh", alive: false, exitCode: 1 },
      geometry: { cols: 100, rows: 30 },
    });
    expect(state.worker?.shell?.alive).toBe(false);
    expect(state.terminalGeometry["worker:shell"]).toEqual({ cols: 100, rows: 30 });
  });
});

describe("the project picker", () => {
  it("opens only on a worker, closing the palette that opened it", () => {
    const open = apply(worker(), { type: "palette/open" }, {
      type: "picker/request",
      intent: { kind: "open" },
    });
    expect(open.palette).toBeNull();
    expect(open.picker?.home).toBe("/home/me");
    const state = tui();
    expect(apply(state, { type: "picker/request", intent: { kind: "open" } }).picker).toBeNull();
  });

  it("an applied open closes it, goes to the project and says so", () => {
    const sent = apply(
      worker([]),
      { type: "picker/request", intent: { kind: "open" } },
      {
        type: "picker/sent",
        sent: { kind: "open", path: "/home/me/flightdeck/alpha" },
        seq: 12,
      },
      { type: "command/result", seq: 12, outcome: "applied" },
    );
    expect(sent.picker).toBeNull();
    expect(sent.pendingOpen).toEqual({ path: "/home/me/flightdeck/alpha", projectId: null });
    expect(sent.notice).toBe("Opened /home/me/flightdeck/alpha.");
    expect(apply(sent, { type: "pendingOpen/done" }).pendingOpen).toBeNull();
  });

  it("a refusal stays on the picker's error line", () => {
    const refused = apply(
      worker([]),
      { type: "picker/request", intent: { kind: "open" } },
      { type: "picker/sent", sent: { kind: "browse" }, seq: 3 },
      { type: "command/result", seq: 3, outcome: "rejected", detail: "outside $HOME" },
    );
    expect(refused.picker?.error).toBe("outside $HOME");
    expect(refused.readOnly).toBeNull();
  });

  it("a clone the closed picker started still ends in the notice", () => {
    const state = apply(worker([]), {
      type: "picker/cloneResult",
      result: { cloneId: 4, url: "u", ok: false, path: null, projectId: null, error: "nope" },
    });
    expect(state.notice).toBe("nope");
  });

  it("keeps what was typed", () => {
    const typed = apply(
      worker([]),
      { type: "picker/request", intent: { kind: "open" } },
      { type: "picker/field", field: "url", value: "git@x:r.git" },
      { type: "picker/field", field: "name", value: "r" },
      { type: "picker/toggleHidden" },
    );
    expect(typed.picker?.urlField).toBe("git@x:r.git");
    expect(typed.picker?.nameField).toBe("r");
    expect(typed.picker?.showHidden).toBe(true);
  });
});

describe("host messages", () => {
  it("are the notice, and an error is also a panel", () => {
    const info = apply(worker(), {
      type: "worker/message",
      message: { id: 1, atMs: 1, level: "warning", text: "disk low" },
    });
    expect(info.notice).toBe("Warning: disk low");
    expect(info.readOnly).toBeNull();

    const error = apply(worker(), {
      type: "worker/message",
      message: { id: 2, atMs: 2, level: "error", text: "clone failed" },
    });
    expect(error.notice).toBe("Error: clone failed");
    expect(error.readOnly).toEqual({
      kind: "message",
      title: "Error on the worker",
      text: "clone failed",
    });
  });

  it("do not interrupt the palette: the first line is the notice instead", () => {
    const busy = apply(worker(), { type: "palette/open" }, {
      type: "worker/away",
      messages: [{ id: 1, atMs: 1, level: "info", text: "updated" }],
    });
    expect(busy.readOnly).toBeNull();
    expect(busy.notice).toBe("1 message while you were away");
  });

  it("the kept ones are one 'while you were away' panel", () => {
    const away = apply(worker(), {
      type: "worker/away",
      messages: [
        { id: 1, atMs: 1, level: "info", text: "a" },
        { id: 2, atMs: 2, level: "error", text: "b" },
      ],
    });
    expect(away.readOnly).toEqual({
      kind: "message",
      title: "While you were away",
      text: "2 messages while you were away\na\nError: b",
    });
    expect(apply(worker(), { type: "worker/away", messages: [] }).readOnly).toBeNull();
  });
});

describe("an unclaimed command result", () => {
  it("on a worker: words become the notice, a refusal a panel", () => {
    const noticed = apply(worker(), {
      type: "command/result",
      seq: 99,
      outcome: "applied",
      detail: "Restarted the worker shell.",
    });
    expect(noticed.notice).toBe("Restarted the worker shell.");
    const refused = apply(worker(), {
      type: "command/result",
      seq: 99,
      outcome: "rejected",
      detail: "no",
    });
    expect(refused.readOnly?.kind).toBe("message");
  });

  it("on a TUI host: the silence it has always had", () => {
    const state = tui();
    expect(
      apply(state, { type: "command/result", seq: 99, outcome: "rejected", detail: "no" }),
    ).toBe(state);
  });
});

describe("takeover (W17)", () => {
  it("is terminal: nothing but the user's reclaim leaves it", () => {
    const taken = apply(
      worker(),
      { type: "connection/changed", status: "connected" },
      { type: "connection/takenOver", by: "FlightDeck Desktop (laptop)" },
    );
    expect(taken.connection).toBe("taken_over");
    expect(taken.takenOverBy).toBe("FlightDeck Desktop (laptop)");
    expect(isTerminalConnection(taken)).toBe(true);
    expect(apply(taken, { type: "connection/changed", status: "reconnecting" }).connection).toBe(
      "taken_over",
    );

    const strip = connectionStrip(taken);
    expect(strip.status.text).toBe("taken over");
    expect(strip.action?.kind).toBe("reclaim");
    expect(strip.message).toContain("FlightDeck Desktop (laptop)");

    const back = apply(taken, { type: "connection/reclaim" });
    expect(back.connection).toBe("connecting");
    expect(back.takenOverBy).toBeNull();
    expect(apply(back, { type: "connection/changed", status: "connected" }).connection).toBe(
      "connected",
    );
  });

  it("a reclaim with nothing taken changes nothing", () => {
    const state = worker();
    expect(apply(state, { type: "connection/reclaim" })).toBe(state);
  });
});

describe("the palette on a worker", () => {
  const rows: HostCommand[] = ["quit", "open_project", "pair_phone", "show_git_status"].map(
    (name) => ({
      id: name,
      label: name,
      group: "Global",
      run: { name },
      hostOnly: false,
      answersDialog: false,
      annotation: null,
      target: null,
      refusal: name === "show_git_status" ? null : "refused on a worker",
    }),
  );

  it("hides the host-screen rows and offers Open Project as runnable", () => {
    const state = { ...worker(), commands: rows };
    const offered = paletteInventory(state);
    expect(offered.map((c) => c.id)).toEqual(["open_project", "show_git_status"]);
    expect(offered[0]?.refusal).toBeNull();
  });

  it("is every row, refusals and all, on a TUI host", () => {
    const state = { ...tui(), commands: rows };
    expect(paletteInventory(state).map((c) => c.id)).toEqual([
      "quit",
      "open_project",
      "pair_phone",
      "show_git_status",
    ]);
  });
});

describe("the access screens say where a worker's code comes from", () => {
  const base: AccessState = {
    screen: "code_entry",
    code: "",
    refused: "",
    attemptsRemaining: null,
    lockoutSeconds: null,
    lockoutLengthSeconds: null,
    codeTtlSeconds: 120,
    revokedAgo: null,
  };

  it("keeps the desktop's words when the host does not say", () => {
    expect(accessCopy(base).body).toContain("press Ctrl-g and run Web Interface");
    expect(accessCopy({ ...base, screen: "rejected" }).steps[1]).toContain("Ctrl-g");
  });

  it("names flightdeck worker pair and revoke on a worker", () => {
    const w = { ...base, hostKind: "worker" as const };
    expect(accessCopy(w).body).toBe(
      "On the worker, run flightdeck worker pair in a shell. It prints a four-digit code, good for 120 seconds.",
    );
    expect(accessCopy({ ...w, screen: "rejected" }).steps).toEqual([
      "open a shell on the worker",
      "run flightdeck worker pair for a fresh code",
    ]);
    const revoked = accessCopy({ ...w, screen: "revoked", revokedAgo: "12s ago" });
    expect(revoked.eyebrow).toBe("on the worker");
    expect(revoked.body).toContain("flightdeck worker revoke 12s ago");
    expect(accessCopy({ ...w, screen: "rate_limited" }).detail).toContain(
      "flightdeck worker pair",
    );
  });

  it("the reducer keeps the refusal's host kind, else what the snapshot said", () => {
    const refused = apply(createInitialState(), {
      type: "access/required",
      screen: "code_entry",
      attemptsRemaining: null,
      lockoutSeconds: null,
      lockoutLengthSeconds: null,
      codeTtlSeconds: null,
      hostKind: "worker",
    });
    expect(refused.access?.hostKind).toBe("worker");

    const unsaid = apply(createInitialState(), {
      type: "access/required",
      screen: "code_entry",
      attemptsRemaining: null,
      lockoutSeconds: null,
      lockoutLengthSeconds: null,
      codeTtlSeconds: null,
    });
    expect(unsaid.access).not.toHaveProperty("hostKind");

    const revoked = apply(worker(), {
      type: "connection/shutdown",
      shutdown: { reason: "token_revoked", selfInitiated: false, detail: "", atLabel: "" },
    });
    expect(revoked.access?.hostKind).toBe("worker");
  });
});
