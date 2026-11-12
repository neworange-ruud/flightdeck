/**
 * Downloading files from a worker, as pure functions — the Transfer files
 * panel's Download tab (v9, `remote-control-pz6d`).
 *
 * The person browses the worker's folders under `$HOME` (`browse_dir` with
 * `include_files: true`), ticks files in one folder, and asks; the worker
 * answers `begin_download` with a `download_plan` — a single-use ticket per
 * file, or a refusal — and the browser then fetches each file itself from
 * `GET /download/<ticket>` (`wire/download.ts`), so its own download manager
 * shows the progress. `DownloadState` holds what the tab shows and folds all
 * of that in:
 *
 * ```text
 * browsing ──download──▶ requested(seq) ──plan: tickets──▶ granted ──▶ handed_off
 *    ▲                     │      │                                      │
 *    │   refusal / error   │      └── cancel ──▶ cancelled               │
 *    └─────────────────────┘                        │                    │
 *    └──────────────── download more ◀──────────────┴────────────────────┘
 * ```
 *
 * Mirrors the native client's `DownloadPanel` (`src/web/client/download.rs`),
 * so a browser and FlightDeck Desktop download the same way:
 *
 * - **It sends nothing and touches no file.** `downloadBrowseCommand` and
 *   `downloadRequest` say what to send; `main.ts` sends it and reports the seq
 *   back (`downloadBrowsing`, `downloadRequested`); `nextDownloadHandoff`
 *   names the files to hand to the browser, and `downloadHandedOff` records
 *   that it did.
 * - **An answer to anything but the latest request is dropped**: a slow
 *   listing never repaints over the folder the person moved on to, and a plan
 *   for a cancelled request starts nothing.
 * - **Selection is per folder.** Moving to another folder clears it;
 *   re-listing the same one (Show hidden) keeps what is still there.
 */

import type { ListedFile, PickerEntry, PickerListing } from "./picker";
import { WORKER_COMMAND } from "./worker";

export { DOWNLOAD_FILES_COMMAND, DOWNLOAD_MIN_VERSION, downloadAvailable } from "./worker";

/** How many of its own seqs the tab remembers, so a superseded request's ack
 * is still recognised as the tab's. */
const SEQS_KEPT = 16;

/** Said when Download is pressed with nothing ticked. */
export const NO_SELECTION = "Select at least one file to download.";

/** Said when the link dropped while the worker was being asked. */
export const LINK_LOST = "The connection to the worker dropped; download again.";

/** Said when a request could not be sent at all (`worker::NOT_SENT`). */
export const NOT_SENT = "Not connected to the worker — nothing was sent.";

/** Said when the worker granted none of the files. */
export const GRANTED_NOTHING = "The worker granted nothing to download.";

/** Said for a refusal that came without words. */
export const REFUSED = "The worker refused the download.";

/** Said under the handed-off line when more than one file went out. */
export const MULTIPLE_HINT =
  "If your browser asks whether this site may download multiple files, allow it.";

/** One file of a granted download: what to fetch, and what to call it. */
export interface DownloadGrant {
  readonly name: string;
  readonly size: number;
  readonly ticket: string;
}

/** `ServerMsg::DownloadPlan`, as the tab takes it. */
export interface DownloadPlan {
  readonly seq: number;
  /** The source folder, canonical and absolute on the worker. */
  readonly dir: string;
  readonly files: readonly {
    readonly name: string;
    readonly size: number;
    readonly ticket: string | null;
  }[];
  readonly refusal: string | null;
}

/** What to send for Download: a `begin_download`'s arguments. */
export interface DownloadRequest {
  readonly dir: string;
  readonly names: readonly string[];
}

/** One row of the listing, folders first: what ↑↓ walk. */
export type DownloadRow =
  | { readonly kind: "folder"; readonly entry: PickerEntry }
  | { readonly kind: "file"; readonly file: ListedFile };

/** What the person asked the tab to do; `main.ts` lists what it means. */
export type DownloadIntent =
  /** The tab opened: list the folder it is in (its starting folder first). */
  | { readonly kind: "open" }
  /** List `path`. */
  | { readonly kind: "browse"; readonly path: string }
  | { readonly kind: "up" }
  /** `$HOME`. */
  | { readonly kind: "home" }
  /** Flip Show hidden (the reducer), then list the same folder again. */
  | { readonly kind: "toggle_hidden" };

export type DownloadPhase =
  /** Browsing folders and ticking files. */
  | { readonly kind: "browsing" }
  /** A `begin_download` is out as `seq`. */
  | { readonly kind: "requested"; readonly seq: number }
  /** Tickets in hand; `main.ts` hands them to the browser. */
  | {
      readonly kind: "granted";
      readonly dir: string;
      readonly files: readonly DownloadGrant[];
    }
  /** The browser has the files; its download manager shows the rest. */
  | {
      readonly kind: "handed_off";
      readonly dir: string;
      readonly names: readonly string[];
      readonly bytes: number;
    }
  | { readonly kind: "cancelled" };

export interface DownloadState {
  /** `$HOME` on the worker: the highest the tab goes. */
  readonly home: string;
  /** The folder the tab starts in — the same rule as an upload's. */
  readonly start: string;
  /** The folder listed, once a listing arrived. */
  readonly cwd: string | null;
  /** Its parent, or `null` at `$HOME`. */
  readonly parent: string | null;
  readonly folders: readonly PickerEntry[];
  readonly files: readonly ListedFile[];
  /** The worker stopped listing before the end. */
  readonly truncated: boolean;
  readonly showHidden: boolean;
  /** The files ticked in `cwd`, by name, in listing order. */
  readonly selected: readonly string[];
  /** The row ↑↓ are on, into `downloadRows`. */
  readonly cursor: number;
  /** The seq of the `browse_dir` out now, if any. */
  readonly browse: number | null;
  readonly phase: DownloadPhase;
  /** The last thing worth saying: a refusal, a dropped link. */
  readonly note: string | null;
  /** Every seq the tab sent lately, newest last. */
  readonly mine: readonly number[];
}

/** A tab that will start in `start`, with nothing listed yet. */
export function newDownload(start: string, home: string): DownloadState {
  return {
    home,
    start,
    cwd: null,
    parent: null,
    folders: [],
    files: [],
    truncated: false,
    showHidden: false,
    selected: [],
    cursor: 0,
    browse: null,
    phase: { kind: "browsing" },
    note: null,
    mine: [],
  };
}

export function isBrowsing(download: DownloadState): boolean {
  return download.phase.kind === "browsing";
}

/** Whether a listing is still to come — the tab's `Listing…`. */
export function isListing(download: DownloadState): boolean {
  return download.browse !== null;
}

/** Folders first, then files: the rows ↑↓ walk, in the worker's order. */
export function downloadRows(download: DownloadState): DownloadRow[] {
  return [
    ...download.folders.map((entry) => ({ kind: "folder" as const, entry })),
    ...download.files.map((file) => ({ kind: "file" as const, file })),
  ];
}

/** The total size of the files ticked. */
export function selectedBytes(download: DownloadState): number {
  return download.files
    .filter((f) => download.selected.includes(f.name))
    .reduce((sum, f) => sum + f.size, 0);
}

/** Whether every file listed is ticked (and there is one). */
export function allSelected(download: DownloadState): boolean {
  return (
    download.files.length > 0 &&
    download.files.every((f) => download.selected.includes(f.name))
  );
}

// ---------------------------------------------------------------------------
// Browsing
// ---------------------------------------------------------------------------

/**
 * The folder an intent lists — `null` is `$HOME` — or `undefined` when it
 * lists nothing (Up at `$HOME`, or anything but browsing).
 */
export function downloadBrowseTarget(
  download: DownloadState,
  intent: DownloadIntent,
): string | null | undefined {
  if (!isBrowsing(download)) {
    return undefined;
  }
  switch (intent.kind) {
    case "open":
    case "toggle_hidden": {
      const here = download.cwd ?? download.start;
      return here === "" ? null : here;
    }
    case "browse": {
      const path = intent.path.trim();
      return path === "" ? null : path;
    }
    case "up":
      return download.parent ?? undefined;
    case "home":
      return null;
  }
}

/** `browse_dir` for `path` (`null`: `$HOME`), files included. */
export function downloadBrowseCommand(
  download: DownloadState,
  path: string | null,
): { readonly name: string; readonly args: Readonly<Record<string, unknown>> } {
  const base = { show_hidden: download.showHidden, include_files: true };
  return {
    name: WORKER_COMMAND.BROWSE_DIR,
    args: path === null ? base : { path, ...base },
  };
}

/** Flip Show hidden; `main.ts` then lists the same folder again. */
export function downloadToggleHidden(download: DownloadState): DownloadState {
  if (!isBrowsing(download)) {
    return download;
  }
  return { ...download, showHidden: !download.showHidden };
}

/** A `browse_dir` went out as `seq` (`null`: it could not be sent). */
export function downloadBrowsing(
  download: DownloadState,
  seq: number | null,
): DownloadState {
  if (seq === null) {
    return { ...download, browse: null, note: NOT_SENT };
  }
  return { ...download, browse: seq, mine: remember(download.mine, seq) };
}

/**
 * A listing. Dropped unless it answers the browse out now. Another folder
 * clears the selection and puts the cursor on the first row; the same folder
 * again keeps every ticked file it still lists.
 */
export function downloadListing(
  download: DownloadState,
  listing: PickerListing,
): DownloadState {
  if (download.browse === null || download.browse !== listing.seq) {
    return download;
  }
  const same = download.cwd === listing.path;
  const names = new Set(listing.files.map((f) => f.name));
  const selected = same ? download.selected.filter((n) => names.has(n)) : [];
  const rows = listing.entries.length + listing.files.length;
  return {
    ...download,
    browse: null,
    home: listing.home,
    cwd: listing.path,
    parent: listing.parent,
    folders: listing.entries,
    files: listing.files,
    truncated: listing.truncated,
    selected,
    cursor: same ? Math.min(download.cursor, Math.max(0, rows - 1)) : 0,
    note: null,
  };
}

// ---------------------------------------------------------------------------
// Choosing files
// ---------------------------------------------------------------------------

/** Tick or untick `name`. Only a file listed here, only while browsing. */
export function downloadToggle(download: DownloadState, name: string): DownloadState {
  if (!isBrowsing(download) || !download.files.some((f) => f.name === name)) {
    return download;
  }
  const on = !download.selected.includes(name);
  const selected = download.files
    .map((f) => f.name)
    .filter((n) => (n === name ? on : download.selected.includes(n)));
  return { ...download, selected, note: null };
}

/** Tick every file — or, when every one already is, none. */
export function downloadSelectAll(download: DownloadState): DownloadState {
  if (!isBrowsing(download) || download.files.length === 0) {
    return download;
  }
  return {
    ...download,
    selected: allSelected(download) ? [] : download.files.map((f) => f.name),
    note: null,
  };
}

/** Move the cursor by `delta` rows, stopping at either end. */
export function downloadMove(download: DownloadState, delta: number): DownloadState {
  return downloadCursor(download, download.cursor + delta);
}

/** Put the cursor on row `index`, clamped to the rows there are. */
export function downloadCursor(download: DownloadState, index: number): DownloadState {
  const rows = download.folders.length + download.files.length;
  const cursor = rows === 0 ? 0 : Math.min(Math.max(0, index), rows - 1);
  return cursor === download.cursor ? download : { ...download, cursor };
}

/**
 * What `Enter` on the cursor's row means: a folder is opened (the intent
 * `main.ts` lists), a file is ticked or unticked (the state that follows).
 */
export function downloadActivate(
  download: DownloadState,
):
  | { readonly kind: "browse"; readonly intent: DownloadIntent }
  | { readonly kind: "toggled"; readonly download: DownloadState }
  | null {
  if (!isBrowsing(download)) {
    return null;
  }
  const row = downloadRows(download)[download.cursor];
  if (row === undefined) {
    return null;
  }
  return row.kind === "folder"
    ? { kind: "browse", intent: { kind: "browse", path: row.entry.path } }
    : { kind: "toggled", download: downloadToggle(download, row.file.name) };
}

/** `Space`: tick or untick the file under the cursor. */
export function downloadToggleCursor(download: DownloadState): DownloadState {
  const row = downloadRows(download)[download.cursor];
  return row?.kind === "file" ? downloadToggle(download, row.file.name) : download;
}

// ---------------------------------------------------------------------------
// Asking the worker
// ---------------------------------------------------------------------------

/**
 * What to send for Download: the folder listed and the files ticked — or,
 * when nothing is ticked, the note saying so. `null` outside browsing, or
 * before anything is listed.
 */
export function downloadRequest(
  download: DownloadState,
): DownloadRequest | { readonly note: string } | null {
  if (!isBrowsing(download) || download.cwd === null) {
    return null;
  }
  if (download.selected.length === 0) {
    return { note: NO_SELECTION };
  }
  return { dir: download.cwd, names: [...download.selected] };
}

/** A request as the worker command it is: `begin_download` and its args. */
export function beginDownloadCommand(request: DownloadRequest): {
  readonly name: string;
  readonly args: Readonly<Record<string, unknown>>;
} {
  return {
    name: WORKER_COMMAND.BEGIN_DOWNLOAD,
    args: { dir: request.dir, names: [...request.names] },
  };
}

/** The request went out as `seq` (`null`: it could not be sent). */
export function downloadRequested(
  download: DownloadState,
  seq: number | null,
): DownloadState {
  if (!isBrowsing(download)) {
    return download;
  }
  if (seq === null) {
    return { ...download, note: NOT_SENT };
  }
  return {
    ...download,
    mine: remember(download.mine, seq),
    phase: { kind: "requested", seq },
    note: null,
  };
}

/** Something the tab could not send, said on its note line. */
export function downloadNote(download: DownloadState, note: string): DownloadState {
  return download.note === note ? download : { ...download, note };
}

// ---------------------------------------------------------------------------
// The worker's answers
// ---------------------------------------------------------------------------

/** The worker's plan. Dropped unless it answers the request out now. */
export function downloadPlan(download: DownloadState, plan: DownloadPlan): DownloadState {
  const phase = download.phase;
  if (phase.kind !== "requested" || plan.seq !== phase.seq) {
    return download;
  }
  if (plan.refusal !== null) {
    return { ...download, phase: { kind: "browsing" }, note: plan.refusal };
  }
  const files: DownloadGrant[] = [];
  for (const row of plan.files) {
    if (row.ticket !== null && row.ticket !== "") {
      files.push({ name: row.name, size: row.size, ticket: row.ticket });
    }
  }
  if (files.length === 0) {
    return { ...download, phase: { kind: "browsing" }, note: GRANTED_NOTHING };
  }
  return { ...download, phase: { kind: "granted", dir: plan.dir, files }, note: null };
}

/**
 * An ack (or a refusing error) for one of the tab's requests. A refused
 * download ends with its reason; a refused listing is the note, and the tab
 * stays where it was. `owned` is whether the seq was the tab's at all.
 */
export function downloadAck(
  download: DownloadState,
  seq: number,
  outcome: "applied" | "rejected" | "ignored" | "read_only",
  detail: string | null,
): { readonly download: DownloadState; readonly owned: boolean } {
  if (!download.mine.includes(seq)) {
    return { download, owned: false };
  }
  const refused = outcome === "rejected" || outcome === "read_only";
  if (!refused) {
    return { download, owned: true };
  }
  if (download.browse === seq) {
    return {
      owned: true,
      download: { ...download, browse: null, note: detail ?? "The worker refused that." },
    };
  }
  const phase = download.phase;
  if (phase.kind === "requested" && phase.seq === seq) {
    return {
      owned: true,
      download: { ...download, phase: { kind: "browsing" }, note: detail ?? REFUSED },
    };
  }
  return { download, owned: true };
}

/**
 * The link dropped: nothing out will be answered (commands are not carried
 * across a reconnect). Tickets already handed to the browser carry on — they
 * need no socket.
 */
export function downloadLinkLost(download: DownloadState): DownloadState {
  const requested = download.phase.kind === "requested";
  if (!requested && download.browse === null) {
    return download;
  }
  return {
    ...download,
    browse: null,
    phase: requested ? { kind: "browsing" } : download.phase,
    note: requested ? LINK_LOST : download.note,
  };
}

// ---------------------------------------------------------------------------
// Handing the files to the browser
// ---------------------------------------------------------------------------

/** The files to hand to the browser, once granted. Pure: `downloadHandedOff`
 * marks them handed. */
export function nextDownloadHandoff(
  download: DownloadState,
): { readonly dir: string; readonly files: readonly DownloadGrant[] } | null {
  const phase = download.phase;
  return phase.kind === "granted" ? { dir: phase.dir, files: phase.files } : null;
}

/** `main.ts` started the browser's downloads. */
export function downloadHandedOff(download: DownloadState): DownloadState {
  const phase = download.phase;
  if (phase.kind !== "granted") {
    return download;
  }
  return {
    ...download,
    phase: {
      kind: "handed_off",
      dir: phase.dir,
      names: phase.files.map((f) => f.name),
      bytes: phase.files.reduce((sum, f) => sum + f.size, 0),
    },
  };
}

/** Stop asking. A plan arriving later starts nothing. */
export function downloadCancel(download: DownloadState): DownloadState {
  const kind = download.phase.kind;
  if (kind !== "requested" && kind !== "granted") {
    return download;
  }
  return { ...download, phase: { kind: "cancelled" }, note: null };
}

/** Download more: back to the folder, nothing ticked. */
export function downloadStartOver(download: DownloadState): DownloadState {
  const kind = download.phase.kind;
  if (kind !== "handed_off" && kind !== "cancelled") {
    return download;
  }
  return { ...download, phase: { kind: "browsing" }, selected: [], note: null };
}

function remember(mine: readonly number[], seq: number): number[] {
  const next = [...mine, seq];
  return next.slice(Math.max(0, next.length - SEQS_KEPT));
}

// ---------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------

/** The handed-off headline. */
export function handedOffLine(count: number): string {
  return count === 1
    ? "Sent 1 file to your browser's downloads."
    : `Sent ${count} files to your browser's downloads.`;
}
