import { describe, expect, it } from "vitest";
import { snapshotFromWire } from "../wire/adapt";
import { workerProject, workerSnapshotWire, WORKER_HOME } from "../wire/workerFixture";
import { fixtureSnapshot } from "./fixture";
import { reduce } from "./reducer";
import { createInitialState } from "./types";
import type { AppState } from "./types";
import {
  beginUploadCommand,
  CANCELLED,
  defaultUploadDir,
  doneLine,
  FOLDER_REFUSED,
  GRANTED_NOTHING,
  humanSize,
  LINK_LOST,
  newUpload,
  nextUploadJob,
  NO_FILES,
  NOT_SENT,
  progressBytes,
  REFUSED,
  totalBytes,
  uploadAck,
  uploadAdd,
  uploadAvailable,
  uploadCancel,
  uploadFinished,
  uploadLinkLost,
  uploadPlan,
  uploadProgress,
  uploadRemove,
  uploadRequest,
  uploadRequested,
  uploadResolve,
  uploadSetDir,
  uploadStarted,
  uploadStartOver,
  workerName,
} from "./upload";
import type { LocalFile, Picked, UploadPlan, UploadState } from "./upload";

/**
 * The upload panel's rules (`specs/WORKER_PRD.md` W34, R30), as the native
 * client's `src/web/client/upload/tests.rs` states them — one phase at a time,
 * then through the reducer, so a browser and FlightDeck Desktop upload the
 * same way.
 */

function file(name: string, size = 3): LocalFile {
  return { name, size, source: new Blob(["x".repeat(size)]) };
}

function picked(name: string, size = 3): Picked {
  return { kind: "file", file: file(name, size) };
}

/** A panel into `/home/me/dir` with `a.txt` (3 bytes) and `b.bin` (5). */
function withTwo(): UploadState {
  return uploadAdd(newUpload("/home/me/dir"), [picked("a.txt", 3), picked("b.bin", 5)]);
}

/** `withTwo`, asked as seq 7. */
function requested(seq = 7): UploadState {
  return uploadRequested(withTwo(), seq, "ask");
}

function granted(seq = 7): UploadPlan {
  return {
    seq,
    dir: "/home/me/dir",
    files: [
      { name: "a.txt", finalName: "a.txt", ticket: "t-a", exists: false },
      { name: "b.bin", finalName: "b.bin", ticket: "t-b", exists: false },
    ],
    conflicts: false,
    refusal: null,
  };
}

function uploading(): UploadState {
  return uploadPlan(requested(), granted());
}

describe("choosing", () => {
  it("adds files in order and keeps one per name", () => {
    const upload = uploadAdd(withTwo(), [picked("a.txt", 9), picked("c.md", 1)]);
    expect(upload.files.map((f) => f.name)).toEqual(["a.txt", "b.bin", "c.md"]);
    expect(upload.files[0]?.size).toBe(3);
    expect(upload.note).toBe("a.txt is already in the list.");
    expect(totalBytes(upload)).toBe(9);
  });

  it("refuses folders with one note, however many", () => {
    const upload = uploadAdd(newUpload("~"), [
      { kind: "folder", name: "src" },
      picked("a.txt"),
      { kind: "folder", name: "docs" },
    ]);
    expect(upload.files.map((f) => f.name)).toEqual(["a.txt"]);
    expect(upload.note).toBe(FOLDER_REFUSED);
  });

  it("says why an unreadable pick was skipped", () => {
    const upload = uploadAdd(newUpload("~"), [
      { kind: "unreadable", name: "x", why: "gone" },
    ]);
    expect(upload.files).toEqual([]);
    expect(upload.note).toBe("Could not read x: gone");
  });

  it("clears the note on a clean add, and on a remove", () => {
    const noted = uploadAdd(withTwo(), [picked("a.txt")]);
    expect(noted.note).not.toBeNull();
    expect(uploadAdd(noted, [picked("z")]).note).toBeNull();
    const removed = uploadRemove(noted, 0);
    expect(removed.files.map((f) => f.name)).toEqual(["b.bin"]);
    expect(removed.note).toBeNull();
    expect(uploadRemove(noted, 5)).toBe(noted);
  });

  it("changes nothing once the worker has been asked", () => {
    const upload = requested();
    expect(uploadAdd(upload, [picked("c")])).toBe(upload);
    expect(uploadRemove(upload, 0)).toBe(upload);
    expect(uploadSetDir(upload, "/tmp")).toBe(upload);
  });

  it("will not ask with no files", () => {
    expect(uploadRequest(newUpload("~"))).toEqual({ note: NO_FILES });
  });

  it("asks for the list, the trimmed folder, and `ask` for taken names", () => {
    const upload = uploadSetDir(withTwo(), "  /home/me/x  ");
    const request = uploadRequest(upload);
    expect(request).toEqual({
      dir: "/home/me/x",
      files: [
        { name: "a.txt", size: 3 },
        { name: "b.bin", size: 5 },
      ],
      onConflict: "ask",
    });
    if (request === null || "note" in request) {
      throw new Error("a request");
    }
    expect(beginUploadCommand(request)).toEqual({
      name: "begin_upload",
      args: {
        dir: "/home/me/x",
        files: [
          { name: "a.txt", size: 3 },
          { name: "b.bin", size: 5 },
        ],
        on_conflict: "ask",
      },
    });
  });
});

describe("asking the worker", () => {
  it("goes back to the list when the request could not be sent", () => {
    const upload = uploadRequested(withTwo(), null, "ask");
    expect(upload.phase).toEqual({ kind: "choosing" });
    expect(upload.note).toBe(NOT_SENT);
  });

  it("takes a grant: every file waiting, nothing sending yet", () => {
    const upload = uploading();
    expect(upload.phase.kind).toBe("uploading");
    if (upload.phase.kind !== "uploading") {
      throw new Error("uploading");
    }
    expect(upload.phase.dir).toBe("/home/me/dir");
    expect(upload.phase.current).toBeNull();
    expect(upload.phase.items.map((i) => [i.name, i.ticket, i.total, i.state.kind])).toEqual([
      ["a.txt", "t-a", 3, "waiting"],
      ["b.bin", "t-b", 5, "waiting"],
    ]);
  });

  it("drops a plan for any seq but the one out now", () => {
    const upload = requested(7);
    expect(uploadPlan(upload, granted(6))).toBe(upload);
    expect(uploadPlan(withTwo(), granted(7)).phase.kind).toBe("choosing");
    /** A superseded request's plan, after a newer one went out. */
    const again = uploadRequested(upload, 9, "ask");
    expect(uploadPlan(again, granted(7))).toBe(again);
  });

  it("asks about taken names, listing them in order", () => {
    const upload = uploadPlan(requested(), {
      seq: 7,
      dir: "/home/me/dir",
      files: [
        { name: "a.txt", finalName: "a.txt", ticket: null, exists: true },
        { name: "b.bin", finalName: "b.bin", ticket: null, exists: false },
      ],
      conflicts: true,
      refusal: null,
    });
    expect(upload.phase).toEqual({
      kind: "asking",
      dir: "/home/me/dir",
      conflicts: ["a.txt"],
    });
  });

  it("keep both and replace send the same files again; cancel goes back", () => {
    const asking: UploadState = {
      ...requested(),
      phase: { kind: "asking", dir: "/home/me/dir", conflicts: ["a.txt"] },
    };
    const keep = uploadResolve(asking, "keep_both");
    expect(keep.request?.onConflict).toBe("keep_both");
    expect(keep.request?.files.map((f) => f.name)).toEqual(["a.txt", "b.bin"]);
    expect(keep.upload).toBe(asking);
    expect(uploadResolve(asking, "replace").request?.onConflict).toBe("replace");
    const cancelled = uploadResolve(asking, "cancel");
    expect(cancelled.request).toBeNull();
    expect(cancelled.upload.phase).toEqual({ kind: "choosing" });
    expect(cancelled.upload.files).toHaveLength(2);
    /** Only while asking. */
    expect(uploadResolve(withTwo(), "replace").request).toBeNull();
  });

  it("a keep-both re-request takes the numbered names it is granted", () => {
    const asking: UploadState = {
      ...requested(7),
      phase: { kind: "asking", dir: "/home/me/dir", conflicts: ["a.txt"] },
    };
    const again = uploadRequested(asking, 8, "keep_both");
    expect(again.phase).toEqual({ kind: "requested", seq: 8, onConflict: "keep_both" });
    const plan = granted(8);
    const upload = uploadPlan(again, {
      ...plan,
      files: [
        { name: "a.txt", finalName: "a (1).txt", ticket: "t-a", exists: true },
        { name: "b.bin", finalName: "b.bin", ticket: "t-b", exists: false },
      ],
    });
    if (upload.phase.kind !== "uploading") {
      throw new Error("uploading");
    }
    expect(upload.phase.items[0]?.finalName).toBe("a (1).txt");
  });

  it("a refusing plan is the note, and back to the list", () => {
    const upload = uploadPlan(requested(), {
      ...granted(),
      files: [],
      refusal: "That folder is outside your home folder.",
    });
    expect(upload.phase).toEqual({ kind: "choosing" });
    expect(upload.note).toBe("That folder is outside your home folder.");
    expect(upload.files).toHaveLength(2);
  });

  it("a plan granting nothing says so", () => {
    const upload = uploadPlan(requested(), {
      ...granted(),
      files: [{ name: "ghost", finalName: "ghost", ticket: "t", exists: false }],
    });
    expect(upload.phase).toEqual({ kind: "choosing" });
    expect(upload.note).toBe(GRANTED_NOTHING);
  });

  it("a rejected ack ends the request with its reason; an applied one waits", () => {
    const upload = requested(7);
    const applied = uploadAck(upload, 7, "applied", null);
    expect(applied).toEqual({ upload, owned: true });
    const rejected = uploadAck(upload, 7, "rejected", "No space left.");
    expect(rejected.owned).toBe(true);
    expect(rejected.upload.phase).toEqual({ kind: "choosing" });
    expect(rejected.upload.note).toBe("No space left.");
    expect(uploadAck(upload, 7, "read_only", null).upload.note).toBe(REFUSED);
  });

  it("owns its superseded seqs, and no one else's", () => {
    const upload = uploadRequested(requested(7), 9, "ask");
    const old = uploadAck(upload, 7, "rejected", "late");
    expect(old.owned).toBe(true);
    expect(old.upload).toBe(upload);
    expect(uploadAck(upload, 8, "rejected", "x").owned).toBe(false);
  });

  it("losing the link while asking goes back to the list", () => {
    const upload = uploadLinkLost(requested());
    expect(upload.phase).toEqual({ kind: "choosing" });
    expect(upload.note).toBe(LINK_LOST);
  });

  it("losing the link while uploading changes nothing: tickets need no socket", () => {
    const upload = uploading();
    expect(uploadLinkLost(upload)).toBe(upload);
  });
});

describe("the transfer", () => {
  it("sends one file at a time, in list order", () => {
    let upload = uploading();
    const first = nextUploadJob(upload);
    expect(first).toMatchObject({ index: 0, ticket: "t-a", size: 3 });
    upload = uploadStarted(upload, 0);
    expect(nextUploadJob(upload)).toBeNull();
    upload = uploadProgress(upload, 0, 2);
    expect(progressBytes(upload)).toEqual({ sent: 2, total: 8 });
    upload = uploadFinished(upload, 0, { ok: true, path: "/home/me/dir/a.txt" });
    expect(nextUploadJob(upload)).toMatchObject({ index: 1, ticket: "t-b", size: 5 });
    expect(progressBytes(upload)).toEqual({ sent: 3, total: 8 });
  });

  it("ignores progress and ends for a file not sending", () => {
    const upload = uploading();
    expect(uploadProgress(upload, 0, 1)).toBe(upload);
    expect(uploadFinished(upload, 0, { ok: true, path: "p" })).toBe(upload);
    expect(uploadStarted(uploadStarted(upload, 0), 1)).toEqual(uploadStarted(upload, 0));
  });

  it("clamps progress to the file's size", () => {
    const upload = uploadProgress(uploadStarted(uploading(), 0), 0, 999);
    expect(progressBytes(upload)?.sent).toBe(3);
  });

  it("ends in done, with every failure's reason", () => {
    let upload = uploadStarted(uploading(), 0);
    upload = uploadFinished(upload, 0, { ok: true, path: "/home/me/dir/a.txt" });
    upload = uploadStarted(upload, 1);
    upload = uploadFinished(upload, 1, { ok: false, why: "No space left on the worker." });
    expect(upload.phase).toEqual({
      kind: "done",
      dir: "/home/me/dir",
      written: ["a.txt"],
      failed: [{ name: "b.bin", why: "No space left on the worker." }],
    });
  });

  it("cancel says whether a transfer was under way, and what is written stays", () => {
    expect(uploadCancel(uploading()).sending).toBe(false);
    const sending = uploadCancel(uploadStarted(uploading(), 0));
    expect(sending.sending).toBe(true);
    expect(sending.upload.phase).toEqual({ kind: "cancelled" });
    /** A transfer's end after a cancel changes nothing. */
    expect(uploadFinished(sending.upload, 0, { ok: false, why: CANCELLED })).toBe(
      sending.upload,
    );
    expect(uploadCancel(requested()).upload.phase).toEqual({ kind: "cancelled" });
  });

  it("starting over keeps the folder and empties the list", () => {
    const upload = uploadStartOver(uploadCancel(uploading()).upload);
    expect(upload.phase).toEqual({ kind: "choosing" });
    expect(upload.files).toEqual([]);
    expect(upload.dir).toBe("/home/me/dir");
  });
});

describe("words", () => {
  it("sizes files the way people read them", () => {
    expect(humanSize(0)).toBe("0 bytes");
    expect(humanSize(1)).toBe("1 byte");
    expect(humanSize(1023)).toBe("1023 bytes");
    expect(humanSize(1536)).toBe("1.5 KB");
    expect(humanSize(20 * 1024 * 1024)).toBe("20 MB");
    expect(humanSize(3 * 1024 ** 3)).toBe("3.0 GB");
  });

  it("sums up a finished upload", () => {
    expect(doneLine(2, 0, "~/x")).toBe("Uploaded 2 files to ~/x.");
    expect(doneLine(1, 1, "~/x")).toBe("Uploaded 1 file to ~/x; 1 failed.");
    expect(doneLine(0, 1, "~/x")).toBe("The file was not uploaded.");
    expect(doneLine(0, 3, "~/x")).toBe("None of the 3 files were uploaded.");
  });
});

// ---------------------------------------------------------------------------
// Through the reducer
// ---------------------------------------------------------------------------

function worker(overrides: Parameters<typeof workerSnapshotWire>[0] = {}): AppState {
  return reduce(createInitialState(), {
    type: "snapshot/received",
    snapshot: snapshotFromWire(workerSnapshotWire(overrides)),
  });
}

describe("the default folder", () => {
  it("is the selected session's worktree", () => {
    const project = workerProject();
    const session = project.sessions[0];
    if (session === undefined) {
      throw new Error("a session");
    }
    const state = worker({
      projects: [
        { ...project, sessions: [{ ...session, worktree_path: "/home/me/wt/fix-login" }] },
      ],
      selection: { project_id: "p-alpha", session_id: "s-1" },
    });
    expect(defaultUploadDir(state)).toBe("/home/me/wt/fix-login");
  });

  it("else the selected project's root", () => {
    const state = worker({
      projects: [workerProject()],
      selection: { project_id: "p-alpha", session_id: "s-1" },
    });
    expect(defaultUploadDir(state)).toBe(`${WORKER_HOME}/flightdeck/alpha`);
  });

  it("else the worker's home, else ~", () => {
    expect(defaultUploadDir(worker())).toBe(WORKER_HOME);
    expect(defaultUploadDir(createInitialState())).toBe("~");
  });
});

describe("the reducer", () => {
  it("opens only on a worker spoken to at v8", () => {
    const open = { type: "upload/open" as const, picked: [picked("a.txt")] };
    expect(uploadAvailable(worker())).toBe(true);
    expect(reduce(worker(), open).upload?.files).toHaveLength(1);
    const v7 = worker({ protocol_version: 7 });
    expect(uploadAvailable(v7)).toBe(false);
    expect(reduce(v7, open).upload).toBeNull();
    const tui = reduce(createInitialState(), {
      type: "snapshot/received",
      snapshot: fixtureSnapshot(),
    });
    expect(uploadAvailable(tui)).toBe(false);
    expect(reduce(tui, open).upload).toBeNull();
  });

  it("opens at the default folder and adds to an open panel", () => {
    let state = reduce(worker(), { type: "upload/open", picked: [picked("a.txt")] });
    expect(state.upload?.dir).toBe(WORKER_HOME);
    state = reduce(state, { type: "upload/open", picked: [picked("b.txt")] });
    expect(state.upload?.files.map((f) => f.name)).toEqual(["a.txt", "b.txt"]);
  });

  it("routes its acks before anyone else's, and a refusal is its note", () => {
    let state = reduce(worker(), { type: "upload/open", picked: [picked("a.txt")] });
    state = reduce(state, { type: "upload/requested", seq: 4, onConflict: "ask" });
    state = reduce(state, {
      type: "command/result",
      seq: 4,
      outcome: "rejected",
      detail: "Only the window holding the seat can upload.",
    });
    expect(state.upload?.phase).toEqual({ kind: "choosing" });
    expect(state.upload?.note).toBe("Only the window holding the seat can upload.");
  });

  it("goes back to the list when the link drops while asking", () => {
    let state = reduce(worker(), { type: "upload/open", picked: [picked("a.txt")] });
    state = reduce(state, { type: "upload/requested", seq: 4, onConflict: "ask" });
    state = reduce(state, { type: "connection/changed", status: "reconnecting" });
    expect(state.upload?.note).toBe(LINK_LOST);
  });

  it("will not close while uploading, and closes otherwise", () => {
    let state = reduce(worker(), { type: "upload/open", picked: [picked("a.txt")] });
    state = reduce(state, { type: "upload/requested", seq: 4, onConflict: "ask" });
    state = reduce(state, {
      type: "upload/plan",
      plan: {
        seq: 4,
        dir: WORKER_HOME,
        files: [{ name: "a.txt", finalName: "a.txt", ticket: "t", exists: false }],
        conflicts: false,
        refusal: null,
      },
    });
    expect(state.upload?.phase.kind).toBe("uploading");
    expect(reduce(state, { type: "upload/close" })).toBe(state);
    state = reduce(state, { type: "upload/cancel" });
    expect(reduce(state, { type: "upload/close" }).upload).toBeNull();
  });

  it("Choose… opens a browse-only picker whose answer is the folder", () => {
    let state = reduce(worker(), { type: "upload/open", picked: [picked("a.txt")] });
    state = reduce(state, {
      type: "picker/request",
      intent: { kind: "choose_folder", path: WORKER_HOME },
    });
    expect(state.picker?.purpose).toBe("choose_folder");
    state = reduce(state, { type: "picker/chosen", path: `${WORKER_HOME}/data` });
    expect(state.picker).toBeNull();
    expect(state.upload?.dir).toBe(`${WORKER_HOME}/data`);
  });

  it("drops the panel on a snapshot from anything but a worker", () => {
    const state = reduce(worker(), { type: "upload/open", picked: [picked("a.txt")] });
    const tui = reduce(state, { type: "snapshot/received", snapshot: fixtureSnapshot() });
    expect(tui.upload).toBeNull();
  });
});

describe("workerName", () => {
  it("is the advertised name without its port, else the host", () => {
    expect(workerName(worker())).toBe("box.tail1234.ts.net");
    expect(workerName({ ...createInitialState(), host: "127.0.0.1:5000" })).toBe("127.0.0.1");
    expect(workerName(createInitialState())).toBe("the worker");
  });
});
