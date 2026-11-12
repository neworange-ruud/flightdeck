import { describe, expect, it } from "vitest";
import { snapshotFromWire } from "../wire/adapt";
import { workerProject, workerSnapshotWire } from "../wire/workerFixture";
import { fixtureSnapshot } from "./fixture";
import type { HostCommand, HostMessage, WorkerHealth } from "./model";
import { reduce } from "./reducer";
import { createInitialState } from "./types";
import type { AppState } from "./types";
import {
  awayText,
  cellsFor,
  diskBytes,
  healthLine,
  messageLine,
  MIN_COLS,
  MIN_ROWS,
  offersNotifications,
  onScreenTerminalId,
  shellExitLine,
  stageGeometry,
  workerCard,
  workerPaletteRow,
} from "./worker";

/**
 * A worker's window, as pure rules (`remote-control-vjtb`). Each of these is
 * a rule the native client states in `session.rs` / `messages.rs`; the
 * strings are asserted verbatim where the native tests assert them, so the
 * two surfaces cannot drift into saying different things.
 */

function worker(overrides: Partial<AppState> = {}): AppState {
  const state = reduce(createInitialState(), {
    type: "snapshot/received",
    snapshot: snapshotFromWire(workerSnapshotWire({ projects: [workerProject()] })),
  });
  return { ...state, ...overrides };
}

function tui(overrides: Partial<AppState> = {}): AppState {
  const state = reduce(createInitialState(), {
    type: "snapshot/received",
    snapshot: fixtureSnapshot(),
  });
  return { ...state, ...overrides };
}

function row(name: string, refusal: string | null = null): HostCommand {
  return {
    id: name,
    label: name,
    group: "Global",
    run: { name },
    hostOnly: false,
    answersDialog: false,
    annotation: null,
    target: null,
    refusal,
  };
}

describe("workerPaletteRow", () => {
  it("hides the rows a worker refuses about a host screen it does not have", () => {
    for (const name of [
      "quit",
      "start_web_interface",
      "stop_web_interface",
      "show_web_access",
      "pair_phone",
      "unpair_phone",
    ]) {
      expect(workerPaletteRow(worker(), row(name, "refused"))).toBeNull();
    }
  });

  it("keeps open_project and lifts its refusal: this window runs it as the picker", () => {
    const offered = workerPaletteRow(worker(), {
      ...row("open_project", "Use the worker's project picker"),
      hostOnly: true,
    });
    expect(offered?.refusal).toBeNull();
    expect(offered?.hostOnly).toBe(false);
  });

  it("lifts the transfer rows only where this window can run them", () => {
    const refusal = "Use the window's Transfer Files panel.";
    for (const name of ["upload_files", "download_files"]) {
      expect(workerPaletteRow(worker(), row(name, refusal))?.refusal).toBeNull();
    }
    /** v8: Upload, and no Download. */
    const v8 = worker({ protocolVersion: 8 });
    expect(workerPaletteRow(v8, row("upload_files", refusal))?.refusal).toBeNull();
    expect(workerPaletteRow(v8, row("download_files", refusal))).toBeNull();
    /** v7: neither. */
    const v7 = worker({ protocolVersion: 7 });
    expect(workerPaletteRow(v7, row("upload_files", refusal))).toBeNull();
    expect(workerPaletteRow(v7, row("download_files", refusal))).toBeNull();
  });

  it("leaves every row on any other host untouched", () => {
    const quit = row("quit", "refused");
    expect(workerPaletteRow(tui(), quit)).toBe(quit);
    const open = row("open_project", "refused");
    expect(workerPaletteRow(tui(), open)).toBe(open);
  });

  it("leaves a row the worker runs untouched", () => {
    const status = row("show_git_status");
    expect(workerPaletteRow(worker(), status)).toBe(status);
  });
});

describe("onScreenTerminalId", () => {
  it("is the selection's terminal on any host but a worker — the empty id included", () => {
    expect(onScreenTerminalId(tui())).toBe(tui().selection?.terminalId);
    const blank = tui({ selection: { projectId: "", sessionId: "", terminalId: "" } });
    expect(onScreenTerminalId(blank)).toBe("");
  });

  it("is the worker shell on its tab, and the selection's terminal otherwise", () => {
    expect(onScreenTerminalId(worker())).toBe("t-agent");
    expect(onScreenTerminalId(worker({ workerShellTab: true }))).toBe("worker:shell");
  });

  it("is nothing on a worker with nothing to show", () => {
    const empty = reduce(createInitialState(), {
      type: "snapshot/received",
      snapshot: snapshotFromWire(workerSnapshotWire({ worker: { ...workerSnapshotWire().worker!, shell: null } })),
    });
    expect(onScreenTerminalId(empty)).toBeNull();
    expect(onScreenTerminalId({ ...empty, workerShellTab: true })).toBeNull();
  });
});

describe("stageGeometry", () => {
  it("is D4's one grid on any host but a worker", () => {
    const state = tui({ terminalGeometry: { t: { cols: 1, rows: 1 } } });
    expect(stageGeometry(state, "t")).toBe(state.geometry);
  });

  it("is each terminal's own grid on a worker, the shared one only as a fallback", () => {
    const state = worker();
    expect(stageGeometry(state, "t-agent")).toEqual({ cols: 132, rows: 40 });
    expect(stageGeometry(state, "worker:shell")).toEqual({ cols: 80, rows: 24 });
    expect(stageGeometry(state, "unknown")).toEqual(state.geometry);
  });
});

describe("cellsFor (W5)", () => {
  it("floors, so the grid never overflows the pane it was measured from", () => {
    expect(cellsFor({ width: 1000, height: 600 }, { width: 9.5, height: 17 })).toEqual({
      cols: 105,
      rows: 35,
    });
  });

  it("never asks for less than a usable shell", () => {
    expect(cellsFor({ width: 10, height: 10 }, { width: 9, height: 17 })).toEqual({
      cols: MIN_COLS,
      rows: MIN_ROWS,
    });
  });

  it("refuses a measurement that cannot be a pane", () => {
    expect(cellsFor({ width: 0, height: 600 }, { width: 9, height: 17 })).toBeNull();
    expect(cellsFor({ width: 800, height: 600 }, { width: 0, height: 17 })).toBeNull();
    expect(cellsFor({ width: 800, height: 600 }, { width: Number.NaN, height: 17 })).toBeNull();
  });
});

describe("workerCard", () => {
  it("is nothing on a TUI or Desktop host, whatever it shows", () => {
    expect(workerCard(tui({ projects: [] }))).toBeNull();
  });

  it("says a worker has no projects yet, on a project tab", () => {
    const empty = reduce(createInitialState(), {
      type: "snapshot/received",
      snapshot: snapshotFromWire(workerSnapshotWire()),
    });
    expect(workerCard(empty)).toBe("empty");
    expect(workerCard(worker())).toBeNull();
  });

  it("offers a restart for a shell that is missing or exited", () => {
    const shell = worker({ workerShellTab: true });
    expect(workerCard(shell)).toBeNull();
    const missing = { ...shell, worker: { ...shell.worker!, shell: null } };
    expect(workerCard(missing)).toBe("shell_missing");
    const exited = {
      ...shell,
      worker: {
        ...shell.worker!,
        shell: { ...shell.worker!.shell!, alive: false, exitCode: 2 },
      },
    };
    expect(workerCard(exited)).toBe("shell_exited");
    expect(shellExitLine(exited.worker.shell)).toBe("The worker shell exited (2).");
    expect(shellExitLine({ ...exited.worker.shell, exitCode: null })).toBe(
      "The worker shell exited.",
    );
    expect(shellExitLine(null)).toBe("The worker shell is not running.");
  });
});

describe("host messages, in messages.rs's words", () => {
  const message = (id: number, level: HostMessage["level"], text: string): HostMessage => ({
    id,
    atMs: id,
    level,
    text,
  });

  it("prefixes a level louder than information", () => {
    expect(messageLine(message(1, "info", "FlightDeck 2.0 is available."))).toBe(
      "FlightDeck 2.0 is available.",
    );
    expect(messageLine(message(1, "warning", "x"))).toBe("Warning: x");
    expect(messageLine(message(1, "error", "y"))).toBe("Error: y");
  });

  it("counts and lists the kept messages oldest first", () => {
    expect(
      awayText([
        message(1, "info", "FlightDeck 2.0 is available."),
        message(2, "error", "Clone failed: repository not found"),
      ]),
    ).toBe(
      "2 messages while you were away\nFlightDeck 2.0 is available.\nError: Clone failed: repository not found",
    );
    expect(awayText([message(1, "warning", "x")])).toBe(
      "1 message while you were away\nWarning: x",
    );
  });
});

describe("healthLine", () => {
  const health: WorkerHealth = {
    version: "1.0.0",
    uptimeSecs: 1,
    loadCenti: 42,
    cpuCount: 8,
    diskFreeHomeBytes: 20e9,
    diskFreeProjectsRootBytes: 18e9,
    agentsTotal: 3,
    agentsRunning: 1,
  };

  it("reads load, free disk under the projects root, and agents", () => {
    expect(healthLine(health)).toBe("load 0.42 · 18 GB free · 3 agents");
  });

  it("leaves out what the worker could not measure, and says one agent", () => {
    expect(
      healthLine({
        ...health,
        loadCenti: null,
        diskFreeProjectsRootBytes: null,
        agentsTotal: 1,
      }),
    ).toBe("20 GB free · 1 agent");
    expect(
      healthLine({
        ...health,
        loadCenti: 105,
        diskFreeHomeBytes: null,
        diskFreeProjectsRootBytes: null,
        agentsTotal: 0,
      }),
    ).toBe("load 1.05");
  });

  it("sizes disks the way people read them", () => {
    expect(diskBytes(18_400_000_000)).toBe("18 GB");
    expect(diskBytes(2_340_000_000)).toBe("2.3 GB");
    expect(diskBytes(512_000_000)).toBe("512 MB");
  });
});

describe("offersNotifications", () => {
  it("only on a worker, and only while the browser has not been answered", () => {
    expect(offersNotifications(worker({ notifications: "default" }))).toBe(true);
    expect(offersNotifications(worker({ notifications: "granted" }))).toBe(false);
    expect(offersNotifications(worker({ notifications: "unsupported" }))).toBe(false);
    expect(offersNotifications(tui({ notifications: "default" }))).toBe(false);
  });
});
