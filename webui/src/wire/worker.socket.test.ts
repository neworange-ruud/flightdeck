import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createStore } from "../ui/store";
import { createInitialState } from "../state/types";
import { fixtureSnapshot } from "../state/fixture";
import type { WireNotificationView, WireSnapshot } from "./frames";
import { openSession } from "./socket";
import { workerProject, workerSnapshotWire, workerWire } from "./workerFixture";

/**
 * The transport's half of a worker's window (`remote-control-vjtb`), driven
 * through an injected socket like `socket.test.ts`: takeover is terminal and
 * only `reconnect()` redials, W5's resize goes only to a worker and only when
 * the size changed, and the worker's frames land in the store instead of
 * costing a resync.
 */

interface FakeSocket {
  onopen: (() => void) | null;
  onmessage: ((event: { data: string }) => void) | null;
  onclose: (() => void) | null;
  onerror: (() => void) | null;
  readyState: number;
  readonly sent: Record<string, unknown>[];
  send(data: string): void;
  close(): void;
}

function fakeSocket(): FakeSocket {
  const socket: FakeSocket = {
    onopen: null,
    onmessage: null,
    onclose: null,
    onerror: null,
    readyState: 1,
    sent: [],
    send(data: string) {
      socket.sent.push(JSON.parse(data) as Record<string, unknown>);
    },
    close() {
      socket.readyState = 3;
    },
  };
  return socket;
}

function harness(options: { onNotification?: (n: WireNotificationView) => void } = {}) {
  const store = createStore(createInitialState());
  const sockets: FakeSocket[] = [];
  const session = openSession({
    store,
    url: "ws://test/ws",
    socketFactory: () => {
      const ws = fakeSocket();
      sockets.push(ws);
      return ws as unknown as WebSocket;
    },
    ...(options.onNotification === undefined
      ? {}
      : { onNotification: options.onNotification }),
  });
  const current = (): FakeSocket => {
    const ws = sockets[sockets.length - 1];
    if (ws === undefined) {
      throw new Error("no socket");
    }
    return ws;
  };
  const deliver = (frame: unknown): void => {
    current().onmessage?.({ data: JSON.stringify(frame) });
  };
  const attach = (snapshot: WireSnapshot = workerSnapshotWire()): void => {
    current().onopen?.();
    deliver(snapshot);
  };
  const sentOfType = (type: string): Record<string, unknown>[] =>
    sockets.flatMap((ws) => ws.sent).filter((frame) => frame.type === type);
  return { store, session, sockets, current, deliver, attach, sentOfType };
}

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

describe("takeover is terminal (W17)", () => {
  it("does not redial after taken_over, and reconnect() takes the worker back", () => {
    const h = harness();
    h.attach();
    h.deliver({ type: "taken_over", by: "FlightDeck Desktop (laptop)" });
    h.current().onclose?.();

    expect(h.store.getState().connection).toBe("taken_over");
    expect(h.store.getState().takenOverBy).toBe("FlightDeck Desktop (laptop)");
    vi.advanceTimersByTime(60_000);
    expect(h.sockets).toHaveLength(1);

    h.session.reconnect();
    expect(h.sockets).toHaveLength(2);
    expect(h.store.getState().connection).toBe("connecting");
    h.attach();
    expect(h.sentOfType("attach")).toHaveLength(2);
    expect(h.store.getState().connection).toBe("connected");
  });

  it("an ordinary close still retries, as it always has", () => {
    const h = harness();
    h.attach();
    h.current().onclose?.();
    vi.advanceTimersByTime(1_000);
    expect(h.sockets.length).toBeGreaterThan(1);
  });
});

describe("resize_terminal (W5)", () => {
  it("is sent to a worker for the terminal on screen, once per size", () => {
    const h = harness();
    h.attach();
    expect(h.session.resizeTerminal("worker:shell", 120, 40)).toBe(true);
    expect(h.session.resizeTerminal("worker:shell", 120, 40)).toBe(false);
    expect(h.session.resizeTerminal("worker:shell", 100, 40)).toBe(true);
    expect(h.session.resizeTerminal("t-agent", 100, 40)).toBe(true);
    expect(h.sentOfType("resize_terminal")).toEqual([
      { type: "resize_terminal", terminal_id: "worker:shell", cols: 120, rows: 40 },
      { type: "resize_terminal", terminal_id: "worker:shell", cols: 100, rows: 40 },
      { type: "resize_terminal", terminal_id: "t-agent", cols: 100, rows: 40 },
    ]);
  });

  it("is sent again after a reattach — whoever held the worker may have resized it", () => {
    const h = harness();
    h.attach();
    h.session.resizeTerminal("worker:shell", 120, 40);
    h.deliver({ type: "taken_over", by: "x" });
    h.session.reconnect();
    h.attach();
    expect(h.session.resizeTerminal("worker:shell", 120, 40)).toBe(true);
  });

  it("is never sent to a TUI or Desktop host (D4)", () => {
    const h = harness();
    h.attach({ ...fixtureWire(), host_kind: "tui" });
    expect(h.session.resizeTerminal("t1", 120, 40)).toBe(false);
    expect(h.sentOfType("resize_terminal")).toEqual([]);
  });
});

describe("a worker's deltas land in the store", () => {
  it("geometry is per terminal on a worker, and D4's one grid elsewhere", () => {
    const h = harness();
    h.attach(workerSnapshotWire({ projects: [workerProject()] }));
    h.deliver({
      type: "delta",
      change: "geometry",
      terminal_id: "t-agent",
      geometry: { cols: 150, rows: 50 },
    });
    expect(h.store.getState().terminalGeometry["t-agent"]).toEqual({ cols: 150, rows: 50 });
    expect(h.store.getState().geometry).toEqual({ cols: 80, rows: 24 });

    const tui = harness();
    tui.attach({ ...fixtureWire(), host_kind: "tui" });
    tui.deliver({
      type: "delta",
      change: "geometry",
      terminal_id: "t1",
      geometry: { cols: 150, rows: 50 },
    });
    expect(tui.store.getState().geometry).toEqual({ cols: 150, rows: 50 });
  });

  it("health, the shell, status and connections are applied, not resynced", () => {
    const h = harness();
    h.attach();
    h.deliver({ type: "delta", change: "health", ...workerWire().health, agents_total: 7 });
    h.deliver({
      type: "delta",
      change: "worker_shell",
      terminal_id: "worker:shell",
      title: "Worker shell",
      geometry: { cols: 99, rows: 33 },
      byte_len: 10,
      replay_from: 0,
      alive: false,
      exit_code: 0,
    });
    h.deliver({
      type: "delta",
      change: "worker_status",
      projects_root: "/home/me/src",
      unread_messages: 2,
    });
    h.deliver({
      type: "delta",
      change: "connections",
      window: { label: "this", since_ms: 1, same_device: true },
      background_links: 2,
    });
    const worker = h.store.getState().worker;
    expect(worker?.health?.agentsTotal).toBe(7);
    expect(worker?.shells).toEqual([
      {
        terminalId: "worker:shell",
        title: "Worker shell",
        alive: false,
        exitCode: 0,
      },
    ]);
    expect(h.store.getState().terminalGeometry["worker:shell"]).toEqual({ cols: 99, rows: 33 });
    expect(worker?.status.projectsRoot).toBe("/home/me/src");
    expect(worker?.connections.backgroundLinks).toBe(2);
    vi.advanceTimersByTime(500);
    /** No resync was asked for any of them. */
    expect(
      h.sentOfType("command").filter((c) => c.name === "request_snapshot"),
    ).toEqual([]);
  });

  it("a host message is the notice once, however often it arrives", () => {
    const h = harness();
    h.attach();
    const message = { type: "delta", change: "host_message", id: 3, at_ms: 9, level: "info", text: "hi" };
    h.deliver(message);
    expect(h.store.getState().notice).toBe("hi");
    h.store.dispatch({ type: "notice/set", notice: null });
    h.deliver(message);
    expect(h.store.getState().notice).toBeNull();
  });

  it("the kept messages are shown once, as one panel, however many snapshots carry them", () => {
    const h = harness();
    const kept = workerSnapshotWire({
      worker: workerWire({
        host_messages: [
          { id: 1, at_ms: 1, level: "info", text: "update available" },
          { id: 2, at_ms: 2, level: "error", text: "clone failed" },
        ],
      }),
    });
    h.attach(kept);
    expect(h.store.getState().readOnly).toEqual({
      kind: "message",
      title: "While you were away",
      text: "2 messages while you were away\nupdate available\nError: clone failed",
    });
    h.store.dispatch({ type: "readOnly/close" });
    h.deliver(kept);
    expect(h.store.getState().readOnly).toBeNull();
  });

  it("a notification is handed to the page, which decides where it goes", () => {
    const seen: WireNotificationView[] = [];
    const h = harness({ onNotification: (n) => seen.push(n) });
    h.attach();
    h.deliver({
      type: "delta",
      change: "notification",
      at_ms: 5,
      title: "alpha needs you",
      body: "fix-login is waiting",
      sound: "input_required",
    });
    expect(seen).toEqual([
      { type: "delta", change: "notification", at_ms: 5, title: "alpha needs you", body: "fix-login is waiting", sound: "input_required" },
    ]);
  });
});

describe("the picker's answers", () => {
  it("reach the picker, and an error naming its request is its refusal", () => {
    const h = harness();
    h.attach();
    h.store.dispatch({ type: "picker/request", intent: { kind: "open" } });
    const seq = h.session.sendCommand("browse_dir", { show_hidden: false });
    h.store.dispatch({ type: "picker/sent", sent: { kind: "browse" }, seq });
    h.deliver({
      type: "dir_listing",
      seq,
      path: "/home/me",
      parent: null,
      home: "/home/me",
      entries: [{ name: "code", path: "/home/me/code", is_git_repo: true }],
    });
    expect(h.store.getState().picker?.entries).toEqual([
      { name: "code", path: "/home/me/code", isGitRepo: true, isOpenProject: false },
    ]);

    const bad = h.session.sendCommand("browse_dir", { path: 4 });
    h.store.dispatch({ type: "picker/sent", sent: { kind: "browse" }, seq: bad });
    h.deliver({ type: "error", code: "bad_request", message: "`path` must be a string", seq: bad });
    expect(h.store.getState().picker?.error).toBe("`path` must be a string");
  });

  it("a scan and a clone's progress reach the picker", () => {
    const h = harness();
    h.attach();
    h.store.dispatch({ type: "picker/request", intent: { kind: "open" } });
    const scan = h.session.sendCommand("scan_projects");
    h.store.dispatch({ type: "picker/sent", sent: { kind: "scan" }, seq: scan });
    h.deliver({
      type: "project_scan",
      seq: scan,
      root: "/home/me/flightdeck",
      root_exists: true,
      repos: [{ name: "alpha", path: "/home/me/flightdeck/alpha" }],
    });
    expect(h.store.getState().picker?.scanned?.repos.map((r) => r.name)).toEqual(["alpha"]);

    const clone = h.session.sendCommand("clone_project", { url: "u" });
    h.store.dispatch({ type: "picker/sent", sent: { kind: "clone", url: "u" }, seq: clone });
    h.deliver({ type: "clone_progress", clone_id: clone, url: "u", phase: "Receiving", percent: 10 });
    expect(h.store.getState().picker?.clone?.percent).toBe(10);
    h.deliver({
      type: "clone_result",
      clone_id: clone,
      url: "u",
      ok: true,
      path: "/home/me/flightdeck/u",
      project_id: "p-u",
    });
    expect(h.store.getState().picker).toBeNull();
    expect(h.store.getState().pendingOpen).toEqual({
      path: "/home/me/flightdeck/u",
      projectId: "p-u",
    });
  });
});

describe("the upload panel's answers (v8)", () => {
  function asking(h: ReturnType<typeof harness>): number {
    h.attach();
    h.store.dispatch({
      type: "upload/open",
      picked: [{ kind: "file", file: { name: "a.txt", size: 3, source: new Blob(["abc"]) } }],
    });
    const seq = h.session.sendCommand("begin_upload", {
      dir: "/home/me",
      files: [{ name: "a.txt", size: 3 }],
      on_conflict: "ask",
    });
    h.store.dispatch({ type: "upload/requested", seq, onConflict: "ask" });
    return seq;
  }

  it("an upload_plan reaches the panel, and its ack does not unsettle it", () => {
    const h = harness();
    const seq = asking(h);
    h.deliver({
      type: "upload_plan",
      seq,
      dir: "/home/me",
      files: [{ name: "a.txt", final_name: "a.txt", ticket: "t-1" }],
    });
    h.deliver({ type: "ack", seq, outcome: "applied" });
    const upload = h.store.getState().upload;
    expect(upload?.phase.kind).toBe("uploading");
    if (upload?.phase.kind !== "uploading") {
      throw new Error("uploading");
    }
    expect(upload.phase.items[0]?.ticket).toBe("t-1");
  });

  it("a conflicts plan asks, naming the taken files", () => {
    const h = harness();
    const seq = asking(h);
    h.deliver({ type: "ack", seq, outcome: "applied", detail: "1 name is taken" });
    h.deliver({
      type: "upload_plan",
      seq,
      dir: "/home/me",
      files: [{ name: "a.txt", final_name: "a.txt", exists: true }],
      conflicts: true,
    });
    expect(h.store.getState().upload?.phase).toEqual({
      kind: "asking",
      dir: "/home/me",
      conflicts: ["a.txt"],
    });
  });

  it("a refusal, by ack or by error, is the panel's note", () => {
    const h = harness();
    const seq = asking(h);
    h.deliver({
      type: "ack",
      seq,
      outcome: "rejected",
      detail: "That folder is outside your home folder.",
    });
    h.deliver({
      type: "upload_plan",
      seq,
      dir: "/home/me",
      files: [],
      refusal: "That folder is outside your home folder.",
    });
    expect(h.store.getState().upload?.phase).toEqual({ kind: "choosing" });
    expect(h.store.getState().upload?.note).toBe("That folder is outside your home folder.");

    const again = h.session.sendCommand("begin_upload", {});
    h.store.dispatch({ type: "upload/requested", seq: again, onConflict: "ask" });
    h.deliver({ type: "error", code: "not_supported", message: "`dir` is missing", seq: again });
    expect(h.store.getState().upload?.note).toBe("`dir` is missing");
  });
});

describe("the Download tab's answers (v9)", () => {
  /** Attached, the Download tab open, its folder listed with two files. */
  function listed(h: ReturnType<typeof harness>): void {
    h.attach();
    h.store.dispatch({ type: "transfer/open", tab: "download" });
    const seq = h.session.sendCommand("browse_dir", {
      path: "/home/me",
      show_hidden: false,
      include_files: true,
    });
    h.store.dispatch({ type: "download/browsing", seq });
    h.deliver({
      type: "dir_listing",
      seq,
      path: "/home/me",
      home: "/home/me",
      entries: [{ name: "code", path: "/home/me/code" }],
      files: [
        { name: "a.txt", path: "/home/me/a.txt", size: 3 },
        { name: "b.bin", path: "/home/me/b.bin", size: 5 },
      ],
    });
    h.deliver({ type: "ack", seq, outcome: "applied" });
  }

  function asking(h: ReturnType<typeof harness>): number {
    listed(h);
    h.store.dispatch({ type: "download/toggle", name: "a.txt" });
    const seq = h.session.sendCommand("begin_download", { dir: "/home/me", names: ["a.txt"] });
    h.store.dispatch({ type: "download/requested", seq });
    return seq;
  }

  it("a dir_listing with files reaches the tab, not the picker", () => {
    const h = harness();
    listed(h);
    const download = h.store.getState().download;
    expect(download?.cwd).toBe("/home/me");
    expect(download?.folders.map((f) => f.name)).toEqual(["code"]);
    expect(download?.files.map((f) => [f.name, f.size])).toEqual([
      ["a.txt", 3],
      ["b.bin", 5],
    ]);
    expect(h.store.getState().picker).toBeNull();
  });

  it("a download_plan grants the tab its tickets, and its ack does not unsettle it", () => {
    const h = harness();
    const seq = asking(h);
    h.deliver({
      type: "download_plan",
      seq,
      dir: "/home/me",
      files: [{ name: "a.txt", size: 3, ticket: "t-1" }],
    });
    h.deliver({ type: "ack", seq, outcome: "applied" });
    expect(h.store.getState().download?.phase).toEqual({
      kind: "granted",
      dir: "/home/me",
      files: [{ name: "a.txt", size: 3, ticket: "t-1" }],
    });
  });

  it("a plan for another seq is dropped", () => {
    const h = harness();
    const seq = asking(h);
    h.deliver({
      type: "download_plan",
      seq: seq + 100,
      dir: "/home/me",
      files: [{ name: "a.txt", size: 3, ticket: "t-x" }],
    });
    expect(h.store.getState().download?.phase).toEqual({ kind: "requested", seq });
  });

  it("a refusal, by plan or by error, is the tab's note", () => {
    const h = harness();
    const seq = asking(h);
    h.deliver({ type: "ack", seq, outcome: "applied" });
    h.deliver({
      type: "download_plan",
      seq,
      dir: "/home/me",
      files: [],
      refusal: "a.txt is not in that folder.",
    });
    expect(h.store.getState().download?.phase).toEqual({ kind: "browsing" });
    expect(h.store.getState().download?.note).toBe("a.txt is not in that folder.");

    const again = h.session.sendCommand("begin_download", {});
    h.store.dispatch({ type: "download/requested", seq: again });
    h.deliver({ type: "error", code: "not_supported", message: "`dir` is missing", seq: again });
    expect(h.store.getState().download?.note).toBe("`dir` is missing");
  });
});

/** The TUI fixture as a wire snapshot — enough of one for a socket to attach. */
function fixtureWire(): WireSnapshot {
  const snapshot = fixtureSnapshot();
  return {
    ...workerSnapshotWire(),
    host_kind: "tui",
    worker: null,
    geometry: { cols: snapshot.geometry.cols, rows: snapshot.geometry.rows },
  };
}
