import { describe, expect, it } from "vitest";
import { checkSession } from "../access/client";
import {
  cloneResultOf,
  dirListingOf,
  hostKindOf,
  hostMessageOf,
  projectScanOf,
  snapshotFromWire,
} from "./adapt";
import type { WireHostMessage } from "./frames";
import { workerProject, workerSnapshotWire, workerWire } from "./workerFixture";

/**
 * Wire → model for a worker's frames (`remote-control-vjtb`). The same rule as
 * the rest of `adapt.ts`: nothing invented, an absence stays an absence, and a
 * word this build does not know falls to the safe reading.
 */

describe("a worker's snapshot", () => {
  it("keeps the host kind, the worker, every grid and each project's root", () => {
    const snapshot = snapshotFromWire(workerSnapshotWire({ projects: [workerProject()] }));
    expect(snapshot.hostKind).toBe("worker");
    expect(snapshot.worker).toEqual({
      home: "/home/me",
      shell: { terminalId: "worker:shell", title: "Worker shell", alive: true, exitCode: null },
      health: {
        version: "9.9.9",
        uptimeSecs: 60,
        loadCenti: 42,
        cpuCount: 8,
        diskFreeHomeBytes: 20_000_000_000,
        diskFreeProjectsRootBytes: 18_000_000_000,
        agentsTotal: 3,
        agentsRunning: 2,
      },
      status: {
        listening: ["100.64.0.1:7420"],
        advertised: "box.tail1234.ts.net:7420",
        notListeningReason: null,
        unreadMessages: 0,
        projectsRoot: "/home/me/flightdeck",
      },
      connections: { window: null, backgroundLinks: 0 },
    });
    expect(snapshot.terminalGeometry).toEqual({
      "t-agent": { cols: 132, rows: 40 },
      "worker:shell": { cols: 80, rows: 24 },
    });
    expect(snapshot.projects[0]?.root).toBe("/home/me/flightdeck/alpha");
  });

  it("a worker with no shell and no health sample says so", () => {
    const snapshot = snapshotFromWire(
      workerSnapshotWire({ worker: workerWire({ shell: null, health: null }) }),
    );
    expect(snapshot.worker?.shell).toBeNull();
    expect(snapshot.worker?.health).toBeNull();
    expect(snapshot.terminalGeometry).toEqual({});
  });

  it("a host that does not say is not a worker", () => {
    const { host_kind: _kind, worker: _worker, ...v6 } = workerSnapshotWire();
    const snapshot = snapshotFromWire(v6);
    expect(snapshot.hostKind).toBeNull();
    expect(snapshot.worker).toBeNull();
  });

  it("reads an unknown host kind as no kind at all", () => {
    expect(hostKindOf("worker")).toBe("worker");
    expect(hostKindOf("tui")).toBe("tui");
    expect(hostKindOf("toaster")).toBeNull();
    expect(hostKindOf(undefined)).toBeNull();
  });
});

describe("a worker's answers", () => {
  it("fills a listing's absences with their honest defaults", () => {
    expect(
      dirListingOf({
        type: "dir_listing",
        seq: 2,
        path: "/home/me",
        home: "/home/me",
        entries: [{ name: "a", path: "/home/me/a" }],
      }),
    ).toEqual({
      seq: 2,
      path: "/home/me",
      parent: null,
      home: "/home/me",
      entries: [{ name: "a", path: "/home/me/a", isGitRepo: false, isOpenProject: false }],
      truncated: false,
    });
  });

  it("keeps a scan's refusal and a clone's failure in the worker's words", () => {
    expect(
      projectScanOf({
        type: "project_scan",
        seq: 1,
        root: "/x",
        root_exists: false,
        repos: [],
        refusal: "outside $HOME",
      }).refusal,
    ).toBe("outside $HOME");
    expect(
      cloneResultOf({ type: "clone_result", clone_id: 1, url: "u", ok: false, error: "git: no" }),
    ).toEqual({ cloneId: 1, url: "u", ok: false, path: null, projectId: null, error: "git: no" });
  });

  it("reads a message level it does not know as information, never as an error", () => {
    const odd = { id: 1, at_ms: 1, level: "shouting", text: "x" } as unknown as WireHostMessage;
    expect(hostMessageOf(odd).level).toBe("info");
  });
});

describe("the access refusal names its host", () => {
  const refuse = (body: Record<string, unknown>) =>
    checkSession({
      fetch: async () =>
        new Response(JSON.stringify({ ok: false, screen: "code_entry", ...body }), {
          status: 401,
          headers: { "content-type": "application/json" },
        }),
    });

  it("reads host_kind when the body carries it", async () => {
    const result = await refuse({ host_kind: "worker" });
    expect(result.ok).toBe(false);
    expect("hostKind" in result ? result.hostKind : undefined).toBe("worker");
  });

  it("is null when the body does not say — the screens then read as before", async () => {
    const result = await refuse({});
    expect("hostKind" in result ? result.hostKind : undefined).toBeNull();
  });
});
