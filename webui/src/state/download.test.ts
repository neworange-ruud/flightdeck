import { describe, expect, it } from "vitest";
import { snapshotFromWire } from "../wire/adapt";
import { workerProject, workerSnapshotWire, WORKER_HOME } from "../wire/workerFixture";
import { fixtureSnapshot } from "./fixture";
import { reduce } from "./reducer";
import { createInitialState } from "./types";
import type { AppState } from "./types";
import {
  allSelected,
  beginDownloadCommand,
  downloadAck,
  downloadActivate,
  downloadAvailable,
  downloadBrowseCommand,
  downloadBrowseTarget,
  downloadBrowsing,
  downloadCancel,
  downloadCursor,
  downloadHandedOff,
  downloadLinkLost,
  downloadListing,
  downloadMove,
  downloadNote,
  downloadPlan,
  downloadRequest,
  downloadRequested,
  downloadRows,
  downloadSelectAll,
  downloadStartOver,
  downloadToggle,
  downloadToggleCursor,
  downloadToggleHidden,
  GRANTED_NOTHING,
  handedOffLine,
  isListing,
  LINK_LOST,
  newDownload,
  nextDownloadHandoff,
  NO_SELECTION,
  NOT_SENT,
  REFUSED,
  selectedBytes,
} from "./download";
import type { DownloadPlan, DownloadState } from "./download";
import type { PickerListing } from "./picker";

/**
 * The Download tab's rules (v9, `remote-control-pz6d`) — one phase at a time,
 * then through the reducer, the way the native client's `DownloadPanel`
 * states them, so a browser and FlightDeck Desktop download the same way.
 */

const HOME = "/home/me";
const DIR = `${HOME}/dir`;

function listing(
  seq: number,
  path: string,
  files: readonly [string, number][] = [
    ["a.txt", 3],
    ["b.bin", 5],
  ],
): PickerListing {
  return {
    seq,
    path,
    parent: path === HOME ? null : HOME,
    home: HOME,
    entries: [{ name: "sub", path: `${path}/sub`, isGitRepo: false, isOpenProject: false }],
    files: files.map(([name, size]) => ({ name, path: `${path}/${name}`, size })),
    truncated: false,
  };
}

/** A tab listing `DIR`: one folder (`sub`), then `a.txt` (3) and `b.bin` (5). */
function listed(): DownloadState {
  return downloadListing(downloadBrowsing(newDownload(DIR, HOME), 1), listing(1, DIR));
}

/** `a.txt` ticked and asked for as seq 9. */
function requested(): DownloadState {
  return downloadRequested(downloadToggle(listed(), "a.txt"), 9);
}

function plan(overrides: Partial<DownloadPlan> = {}): DownloadPlan {
  return {
    seq: 9,
    dir: DIR,
    files: [{ name: "a.txt", size: 3, ticket: "t-a" }],
    refusal: null,
    ...overrides,
  };
}

describe("a new tab", () => {
  it("starts browsing, with nothing listed and nothing ticked", () => {
    const download = newDownload(DIR, HOME);
    expect(download.phase).toEqual({ kind: "browsing" });
    expect(download.cwd).toBeNull();
    expect(download.start).toBe(DIR);
    expect(downloadRows(download)).toEqual([]);
    expect(isListing(download)).toBe(false);
  });
});

describe("browsing", () => {
  it("lists its starting folder first, then the folder it is in", () => {
    const fresh = newDownload(DIR, HOME);
    expect(downloadBrowseTarget(fresh, { kind: "open" })).toBe(DIR);
    expect(downloadBrowseTarget(listed(), { kind: "open" })).toBe(DIR);
    /** No starting folder at all is `$HOME`. */
    expect(downloadBrowseTarget(newDownload("", HOME), { kind: "open" })).toBeNull();
  });

  it("goes into a folder, up to its parent, and home", () => {
    const download = listed();
    expect(downloadBrowseTarget(download, { kind: "browse", path: ` ${DIR}/sub ` })).toBe(
      `${DIR}/sub`,
    );
    expect(downloadBrowseTarget(download, { kind: "up" })).toBe(HOME);
    expect(downloadBrowseTarget(download, { kind: "home" })).toBeNull();
    /** At `$HOME` there is no Up. */
    const atHome = downloadListing(downloadBrowsing(download, 2), listing(2, HOME));
    expect(downloadBrowseTarget(atHome, { kind: "up" })).toBeUndefined();
  });

  it("asks for files, and for hidden ones only when shown", () => {
    const download = listed();
    expect(downloadBrowseCommand(download, DIR)).toEqual({
      name: "browse_dir",
      args: { path: DIR, show_hidden: false, include_files: true },
    });
    const hidden = downloadToggleHidden(download);
    expect(hidden.showHidden).toBe(true);
    expect(downloadBrowseCommand(hidden, null)).toEqual({
      name: "browse_dir",
      args: { show_hidden: true, include_files: true },
    });
    expect(downloadBrowseTarget(hidden, { kind: "toggle_hidden" })).toBe(DIR);
  });

  it("takes only the listing that answers the browse out now", () => {
    let download = downloadBrowsing(newDownload(DIR, HOME), 1);
    expect(isListing(download)).toBe(true);
    download = downloadBrowsing(download, 2);
    /** The first browse was superseded: its listing is stale. */
    expect(downloadListing(download, listing(1, HOME))).toBe(download);
    download = downloadListing(download, listing(2, DIR));
    expect(download.cwd).toBe(DIR);
    expect(download.parent).toBe(HOME);
    expect(download.folders.map((f) => f.name)).toEqual(["sub"]);
    expect(download.files.map((f) => f.name)).toEqual(["a.txt", "b.bin"]);
    expect(isListing(download)).toBe(false);
    /** Nothing out: a listing that arrives anyway changes nothing. */
    expect(downloadListing(download, listing(2, HOME))).toBe(download);
  });

  it("clears the selection on another folder, and keeps it on the same one", () => {
    let download = downloadSelectAll(listed());
    download = downloadCursor(download, 2);
    const again = downloadListing(
      downloadBrowsing(download, 5),
      listing(5, DIR, [["b.bin", 5], ["c", 1]]),
    );
    /** Re-listed (Show hidden): `a.txt` is gone, `b.bin` stays ticked. */
    expect(again.selected).toEqual(["b.bin"]);
    expect(again.cursor).toBe(2);
    const elsewhere = downloadListing(downloadBrowsing(download, 6), listing(6, HOME));
    expect(elsewhere.selected).toEqual([]);
    expect(elsewhere.cursor).toBe(0);
  });

  it("says when a browse could not be sent, or was refused", () => {
    expect(downloadBrowsing(listed(), null).note).toBe(NOT_SENT);
    const out = downloadBrowsing(listed(), 4);
    const refused = downloadAck(out, 4, "rejected", "That folder is outside your home folder.");
    expect(refused.owned).toBe(true);
    expect(refused.download.browse).toBeNull();
    expect(refused.download.note).toBe("That folder is outside your home folder.");
    expect(refused.download.cwd).toBe(DIR);
    /** Applied: the listing itself is the answer. */
    expect(downloadAck(out, 4, "applied", null)).toEqual({ download: out, owned: true });
    /** Not the tab's seq at all. */
    expect(downloadAck(out, 99, "rejected", "x")).toEqual({ download: out, owned: false });
  });
});

describe("choosing files", () => {
  it("ticks and unticks files, in listing order", () => {
    let download = downloadToggle(listed(), "b.bin");
    download = downloadToggle(download, "a.txt");
    expect(download.selected).toEqual(["a.txt", "b.bin"]);
    expect(selectedBytes(download)).toBe(8);
    download = downloadToggle(download, "b.bin");
    expect(download.selected).toEqual(["a.txt"]);
    /** Not a file listed here. */
    expect(downloadToggle(download, "sub")).toBe(download);
  });

  it("selects all, then none", () => {
    const all = downloadSelectAll(listed());
    expect(all.selected).toEqual(["a.txt", "b.bin"]);
    expect(allSelected(all)).toBe(true);
    expect(downloadSelectAll(all).selected).toEqual([]);
    const empty = downloadListing(downloadBrowsing(listed(), 3), listing(3, HOME, []));
    expect(downloadSelectAll(empty)).toBe(empty);
    expect(allSelected(empty)).toBe(false);
  });

  it("walks the rows with the cursor, folders first, stopping at either end", () => {
    let download = listed();
    expect(downloadRows(download).map((r) => r.kind)).toEqual(["folder", "file", "file"]);
    expect(downloadMove(download, -1)).toBe(download);
    download = downloadMove(download, 1);
    expect(download.cursor).toBe(1);
    download = downloadMove(download, 10);
    expect(download.cursor).toBe(2);
    expect(downloadCursor(download, -4).cursor).toBe(0);
  });

  it("Enter opens a folder and ticks a file; Space ticks only a file", () => {
    const onFolder = listed();
    expect(downloadActivate(onFolder)).toEqual({
      kind: "browse",
      intent: { kind: "browse", path: `${DIR}/sub` },
    });
    expect(downloadToggleCursor(onFolder)).toBe(onFolder);
    const onFile = downloadCursor(onFolder, 1);
    const entered = downloadActivate(onFile);
    expect(entered?.kind).toBe("toggled");
    expect(entered?.kind === "toggled" ? entered.download.selected : null).toEqual(["a.txt"]);
    expect(downloadToggleCursor(onFile).selected).toEqual(["a.txt"]);
  });

  it("is locked while a download is being asked for", () => {
    const asking = requested();
    expect(downloadToggle(asking, "b.bin")).toBe(asking);
    expect(downloadSelectAll(asking)).toBe(asking);
    expect(downloadToggleHidden(asking)).toBe(asking);
    expect(downloadActivate(asking)).toBeNull();
    expect(downloadBrowseTarget(asking, { kind: "home" })).toBeUndefined();
  });
});

describe("asking the worker", () => {
  it("asks for the folder listed and the files ticked", () => {
    const download = downloadToggle(downloadToggle(listed(), "b.bin"), "a.txt");
    const request = downloadRequest(download);
    expect(request).toEqual({ dir: DIR, names: ["a.txt", "b.bin"] });
    if (request === null || "note" in request) {
      throw new Error("a request");
    }
    expect(beginDownloadCommand(request)).toEqual({
      name: "begin_download",
      args: { dir: DIR, names: ["a.txt", "b.bin"] },
    });
  });

  it("says why when nothing is ticked, and asks nothing before a listing", () => {
    expect(downloadRequest(listed())).toEqual({ note: NO_SELECTION });
    expect(downloadRequest(newDownload(DIR, HOME))).toBeNull();
    expect(downloadRequest(requested())).toBeNull();
    expect(downloadNote(listed(), NO_SELECTION).note).toBe(NO_SELECTION);
  });

  it("waits for the seq it sent, or says it could not send", () => {
    const asking = requested();
    expect(asking.phase).toEqual({ kind: "requested", seq: 9 });
    expect(asking.mine).toContain(9);
    const unsent = downloadRequested(downloadToggle(listed(), "a.txt"), null);
    expect(unsent.phase).toEqual({ kind: "browsing" });
    expect(unsent.note).toBe(NOT_SENT);
  });
});

describe("the worker's answers", () => {
  it("a plan with tickets is granted, and handed to the browser once", () => {
    const granted = downloadPlan(requested(), plan());
    expect(granted.phase).toEqual({
      kind: "granted",
      dir: DIR,
      files: [{ name: "a.txt", size: 3, ticket: "t-a" }],
    });
    expect(nextDownloadHandoff(granted)).toEqual({
      dir: DIR,
      files: [{ name: "a.txt", size: 3, ticket: "t-a" }],
    });
    const handed = downloadHandedOff(granted);
    expect(handed.phase).toEqual({ kind: "handed_off", dir: DIR, names: ["a.txt"], bytes: 3 });
    expect(nextDownloadHandoff(handed)).toBeNull();
    expect(downloadHandedOff(handed)).toBe(handed);
  });

  it("a refusal goes back to the folder, in the worker's words", () => {
    const refused = downloadPlan(
      requested(),
      plan({ files: [], refusal: "a.txt is not in that folder." }),
    );
    expect(refused.phase).toEqual({ kind: "browsing" });
    expect(refused.note).toBe("a.txt is not in that folder.");
    /** The ticks stay, so trying again is one press. */
    expect(refused.selected).toEqual(["a.txt"]);
  });

  it("a plan that grants nothing says so", () => {
    const none = downloadPlan(
      requested(),
      plan({ files: [{ name: "a.txt", size: 3, ticket: null }] }),
    );
    expect(none.phase).toEqual({ kind: "browsing" });
    expect(none.note).toBe(GRANTED_NOTHING);
  });

  it("a stale plan is dropped, and so is one for a cancelled request", () => {
    const asking = requested();
    expect(downloadPlan(asking, plan({ seq: 8 }))).toBe(asking);
    const cancelled = downloadCancel(asking);
    expect(cancelled.phase).toEqual({ kind: "cancelled" });
    expect(downloadPlan(cancelled, plan())).toBe(cancelled);
    expect(nextDownloadHandoff(cancelled)).toBeNull();
  });

  it("a refused request ends with its reason; an ack after a plan changes nothing", () => {
    const refused = downloadAck(requested(), 9, "read_only", null);
    expect(refused.owned).toBe(true);
    expect(refused.download.phase).toEqual({ kind: "browsing" });
    expect(refused.download.note).toBe(REFUSED);
    const worded = downloadAck(requested(), 9, "rejected", "Only the window holding the seat.");
    expect(worded.download.note).toBe("Only the window holding the seat.");
    const granted = downloadPlan(requested(), plan());
    expect(downloadAck(granted, 9, "applied", null)).toEqual({ download: granted, owned: true });
  });

  it("the link dropping while asking goes back to the folder", () => {
    const lost = downloadLinkLost(requested());
    expect(lost.phase).toEqual({ kind: "browsing" });
    expect(lost.note).toBe(LINK_LOST);
    const listing = downloadLinkLost(downloadBrowsing(listed(), 3));
    expect(listing.browse).toBeNull();
    expect(listing.note).toBeNull();
    const handed = downloadHandedOff(downloadPlan(requested(), plan()));
    expect(downloadLinkLost(handed)).toBe(handed);
  });
});

describe("afterwards", () => {
  it("Download more goes back to the same folder with nothing ticked", () => {
    const handed = downloadHandedOff(downloadPlan(requested(), plan()));
    const more = downloadStartOver(handed);
    expect(more.phase).toEqual({ kind: "browsing" });
    expect(more.cwd).toBe(DIR);
    expect(more.selected).toEqual([]);
    expect(downloadStartOver(downloadCancel(requested())).phase).toEqual({ kind: "browsing" });
    /** Only from the end of one. */
    const browsing = listed();
    expect(downloadStartOver(browsing)).toBe(browsing);
    expect(downloadCancel(browsing)).toBe(browsing);
  });

  it("says how many files went to the browser", () => {
    expect(handedOffLine(1)).toBe("Sent 1 file to your browser's downloads.");
    expect(handedOffLine(3)).toBe("Sent 3 files to your browser's downloads.");
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

/** The panel open on Download, `DIR`'s listing in. */
function onDownload(state: AppState = worker()): AppState {
  let next = reduce(state, { type: "transfer/open", tab: "download" });
  next = reduce(next, { type: "download/browsing", seq: 1 });
  return reduce(next, { type: "picker/listing", listing: listing(1, DIR) });
}

describe("the Transfer files panel", () => {
  it("offers Download only on a worker spoken to at v9", () => {
    expect(downloadAvailable(worker())).toBe(true);
    const v8 = worker({ protocol_version: 8 });
    expect(downloadAvailable(v8)).toBe(false);
    /** Asked for Download at v8, the panel opens on Upload. */
    const opened = reduce(v8, { type: "transfer/open", tab: "download" });
    expect(opened.upload).not.toBeNull();
    expect(opened.download).toBeNull();
    expect(opened.transferTab).toBe("upload");
    const tui = reduce(createInitialState(), {
      type: "snapshot/received",
      snapshot: fixtureSnapshot(),
    });
    expect(downloadAvailable(tui)).toBe(false);
    expect(reduce(tui, { type: "transfer/open", tab: null })).toBe(tui);
  });

  it("opens on the tab asked for, else the tab last used", () => {
    let state = reduce(worker(), { type: "transfer/open", tab: null });
    expect(state.transferTab).toBe("upload");
    expect(state.upload?.dir).toBe(WORKER_HOME);
    state = reduce(state, { type: "transfer/open", tab: "download" });
    expect(state.transferTab).toBe("download");
    expect(state.download?.start).toBe(WORKER_HOME);
    expect(state.download?.home).toBe(WORKER_HOME);
    state = reduce(state, { type: "transfer/close" });
    expect(state.upload).toBeNull();
    expect(state.download).toBeNull();
    state = reduce(state, { type: "transfer/open", tab: null });
    expect(state.transferTab).toBe("download");
    expect(state.download?.cwd).toBeNull();
  });

  it("starts the Download tab where an upload would go", () => {
    const state = reduce(
      worker({
        projects: [workerProject()],
        selection: { project_id: "p-alpha", session_id: "s-1" },
      }),
      { type: "transfer/open", tab: "download" },
    );
    expect(state.download?.start).toBe(`${WORKER_HOME}/flightdeck/alpha`);
  });

  it("closes the palette that opened it", () => {
    let state = reduce(worker(), { type: "palette/open" });
    expect(state.palette).not.toBeNull();
    state = reduce(state, { type: "transfer/open", tab: "download" });
    expect(state.palette).toBeNull();
  });

  it("will not switch tabs while uploading", () => {
    let state = reduce(worker(), {
      type: "upload/open",
      picked: [{ kind: "file", file: { name: "a.txt", size: 3, source: new Blob(["abc"]) } }],
    });
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
    expect(reduce(state, { type: "transfer/open", tab: "download" })).toBe(state);
    expect(reduce(state, { type: "transfer/close" })).toBe(state);
  });

  it("dropped files switch it to Upload", () => {
    let state = onDownload();
    state = reduce(state, {
      type: "upload/open",
      picked: [{ kind: "file", file: { name: "a.txt", size: 3, source: new Blob(["abc"]) } }],
    });
    expect(state.transferTab).toBe("upload");
    expect(state.upload?.files).toHaveLength(1);
    /** The Download tab keeps its place for when it is shown again. */
    expect(state.download?.cwd).toBe(DIR);
  });

  it("drops both tabs on a snapshot from anything but a worker", () => {
    const tui = reduce(onDownload(), { type: "snapshot/received", snapshot: fixtureSnapshot() });
    expect(tui.upload).toBeNull();
    expect(tui.download).toBeNull();
  });
});

describe("the Download tab through the reducer", () => {
  it("takes its listing, and leaves the picker's alone", () => {
    let state = onDownload();
    expect(state.download?.files.map((f) => f.name)).toEqual(["a.txt", "b.bin"]);
    /** The picker asks with its own seq; the tab drops that listing. */
    state = reduce(state, { type: "picker/request", intent: { kind: "open" } });
    state = reduce(state, { type: "picker/sent", sent: { kind: "browse" }, seq: 2 });
    state = reduce(state, { type: "picker/listing", listing: listing(2, HOME) });
    expect(state.picker?.cwd).toBe(HOME);
    expect(state.download?.cwd).toBe(DIR);
  });

  it("ticks, walks, asks, is granted, and hands off", () => {
    let state = onDownload();
    state = reduce(state, { type: "download/move", delta: 1 });
    state = reduce(state, { type: "download/toggleCursor" });
    state = reduce(state, { type: "download/toggle", name: "b.bin" });
    expect(state.download?.selected).toEqual(["a.txt", "b.bin"]);
    state = reduce(state, { type: "download/selectAll" });
    expect(state.download?.selected).toEqual([]);
    state = reduce(state, { type: "download/selectAll" });
    state = reduce(state, { type: "download/cursor", index: 0 });
    expect(state.download?.cursor).toBe(0);
    expect(reduce(state, { type: "download/submit" })).toBe(state);
    state = reduce(state, { type: "download/requested", seq: 7 });
    state = reduce(state, {
      type: "download/plan",
      plan: plan({
        seq: 7,
        files: [
          { name: "a.txt", size: 3, ticket: "t-a" },
          { name: "b.bin", size: 5, ticket: "t-b" },
        ],
      }),
    });
    expect(state.download?.phase.kind).toBe("granted");
    state = reduce(state, { type: "download/handedOff" });
    expect(state.download?.phase).toEqual({
      kind: "handed_off",
      dir: DIR,
      names: ["a.txt", "b.bin"],
      bytes: 8,
    });
    state = reduce(state, { type: "download/startOver" });
    expect(state.download?.phase).toEqual({ kind: "browsing" });
  });

  it("routes its acks before anyone else's, and a refusal is its note", () => {
    let state = onDownload();
    state = reduce(state, { type: "download/toggle", name: "a.txt" });
    state = reduce(state, { type: "download/requested", seq: 7 });
    state = reduce(state, {
      type: "command/result",
      seq: 7,
      outcome: "rejected",
      detail: "Only the window holding the seat can download.",
    });
    expect(state.download?.phase).toEqual({ kind: "browsing" });
    expect(state.download?.note).toBe("Only the window holding the seat can download.");
  });

  it("flips Show hidden on its intent, and nothing else changes locally", () => {
    let state = onDownload();
    state = reduce(state, { type: "download/intent", intent: { kind: "toggle_hidden" } });
    expect(state.download?.showHidden).toBe(true);
    expect(reduce(state, { type: "download/intent", intent: { kind: "up" } })).toBe(state);
  });

  it("says what could not be sent, and cancels a request out", () => {
    let state = onDownload();
    state = reduce(state, { type: "download/note", note: NO_SELECTION });
    expect(state.download?.note).toBe(NO_SELECTION);
    state = reduce(state, { type: "download/browsing", seq: null });
    expect(state.download?.note).toBe(NOT_SENT);
    state = reduce(state, { type: "download/toggle", name: "a.txt" });
    state = reduce(state, { type: "download/requested", seq: 8 });
    state = reduce(state, { type: "download/cancel" });
    expect(state.download?.phase).toEqual({ kind: "cancelled" });
  });

  it("goes back to the folder when the link drops, or the worker is taken", () => {
    let state = onDownload();
    state = reduce(state, { type: "download/toggle", name: "a.txt" });
    state = reduce(state, { type: "download/requested", seq: 8 });
    const dropped = reduce(state, { type: "connection/changed", status: "reconnecting" });
    expect(dropped.download?.note).toBe(LINK_LOST);
    const taken = reduce(state, { type: "connection/takenOver", by: "laptop" });
    expect(taken.download?.phase).toEqual({ kind: "browsing" });
  });
});
