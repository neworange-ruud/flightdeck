/**
 * A worker's project picker, as pure functions (`specs/WORKER_PRD.md` W9, W13,
 * W16; `remote-control-vjtb`).
 *
 * A worker has no screen and no folder dialog, so Open Project on a worker is
 * this panel: scan the projects root for repositories, browse `$HOME` folder by
 * folder, or clone a URL. The worker answers each request with its own frame
 * (`dir_listing`, `project_scan`, `clone_progress`, `clone_result`) beside the
 * `ack` for the command, and refuses anything outside `$HOME` in words.
 *
 * A port of the native client's `ProjectPicker` (`src/web/client/worker.rs`),
 * rule for rule, so a browser and FlightDeck Desktop open projects on a worker
 * the same way:
 *
 * - **It sends nothing itself.** `pickerRequest` says which command a click
 *   means; `main.ts` sends it and reports the seq back (`pickerSent`).
 * - **An answer to a superseded request is dropped**, so a slow listing never
 *   repaints over the folder the person has already moved on to.
 * - **Refusals are the worker's own words**, verbatim, on the error line.
 */

import { WORKER_COMMAND } from "./worker";

/** One subfolder in a listing (`protocol::DirEntryView`). */
export interface PickerEntry {
  readonly name: string;
  readonly path: string;
  /** A git repository, and so openable as a project. */
  readonly isGitRepo: boolean;
  /** Already open on the worker. */
  readonly isOpenProject: boolean;
}

/** One repository found under the projects root (`protocol::ScannedRepo`). */
export interface PickerRepo {
  readonly name: string;
  readonly path: string;
  readonly isOpenProject: boolean;
}

/** The last scan of the projects root. */
export interface PickerScan {
  readonly root: string;
  /** An empty scan of a missing root is a different thing to say than an
   * empty scan of an empty one. */
  readonly rootExists: boolean;
  readonly repos: readonly PickerRepo[];
}

/** `ServerMsg::DirListing`, as the picker takes it. */
export interface PickerListing {
  readonly seq: number;
  readonly path: string;
  readonly parent: string | null;
  readonly home: string;
  readonly entries: readonly PickerEntry[];
  readonly truncated: boolean;
}

/** `ServerMsg::ProjectScan`, as the picker takes it. */
export interface PickerScanAnswer {
  readonly seq: number;
  readonly root: string;
  readonly rootExists: boolean;
  readonly repos: readonly PickerRepo[];
  readonly refusal: string | null;
}

/** `ServerMsg::CloneProgress`. */
export interface CloneProgress {
  readonly cloneId: number;
  readonly url: string;
  readonly phase: string;
  readonly percent: number | null;
}

/** `ServerMsg::CloneResult`. */
export interface CloneResult {
  readonly cloneId: number;
  readonly url: string;
  readonly ok: boolean;
  readonly path: string | null;
  readonly projectId: string | null;
  readonly error: string | null;
}

/** A clone this picker started. `cloneId` is the seq of its `clone_project`. */
export interface PickerClone {
  readonly cloneId: number;
  readonly url: string;
  readonly phase: string;
  readonly percent: number | null;
  /** `ok` once cloned (and opened), `failed` with git's words otherwise. */
  readonly done:
    | { readonly ok: true; readonly path: string }
    | { readonly ok: false; readonly why: string }
    | null;
}

/** A project the worker opened at this picker's request. */
export interface OpenedProject {
  readonly path: string;
  /** Its id, when the worker said (a clone's result does). */
  readonly projectId: string | null;
}

/** How many of its own seqs the picker remembers, so the ack trailing an
 * answer — or a superseded request's — is still recognised as the picker's. */
const SEQS_KEPT = 32;

interface Slot {
  readonly seq: number;
  /** Whether its answer frame is still to come. */
  readonly waiting: boolean;
}

export interface PickerPending {
  readonly browse: Slot | null;
  readonly scan: Slot | null;
  readonly open: { readonly seq: number; readonly path: string } | null;
  /** Every seq the picker sent lately, newest last. */
  readonly mine: readonly number[];
}

export interface PickerState {
  /** `$HOME` on the worker: where browsing starts and the highest it goes. */
  readonly home: string;
  /** The folder listed, once a listing arrived. */
  readonly cwd: string | null;
  /** Its parent, or `null` at `$HOME`. */
  readonly parent: string | null;
  readonly entries: readonly PickerEntry[];
  /** The worker stopped listing before the end. */
  readonly truncated: boolean;
  readonly showHidden: boolean;
  readonly scanned: PickerScan | null;
  /** The three text fields. Held here, not in the DOM, so a re-render cannot
   * lose what was typed. */
  readonly pathField: string;
  readonly urlField: string;
  readonly nameField: string;
  readonly clone: PickerClone | null;
  /** The last refusal or failure, verbatim. */
  readonly error: string | null;
  /** The last success worth saying (`Opened project 'x'.`). */
  readonly status: string | null;
  /** Set when the worker opened a project for this picker; the reducer then
   * closes the picker and goes there. */
  readonly opened: OpenedProject | null;
  readonly pending: PickerPending;
}

export function newPicker(home: string): PickerState {
  return {
    home,
    cwd: null,
    parent: null,
    entries: [],
    truncated: false,
    showHidden: false,
    scanned: null,
    pathField: "",
    urlField: "",
    nameField: "",
    clone: null,
    error: null,
    status: null,
    opened: null,
    pending: { browse: null, scan: null, open: null, mine: [] },
  };
}

/** Whether any request is still unanswered — the panel's `Working…`. */
export function pickerBusy(picker: PickerState): boolean {
  return (
    picker.pending.browse?.waiting === true ||
    picker.pending.scan?.waiting === true ||
    picker.pending.open !== null ||
    (picker.clone !== null && picker.clone.done === null)
  );
}

// ---------------------------------------------------------------------------
// What a click means
// ---------------------------------------------------------------------------

/** What the panel asks for. `main.ts` turns one into a command. */
export type PickerIntent =
  /** Open the panel (or refresh it): scan the root and list the folder. */
  | { readonly kind: "open" }
  /** List `path`; `null` is `$HOME`. */
  | { readonly kind: "browse"; readonly path: string | null }
  /** Up one folder from the one listed. */
  | { readonly kind: "up" }
  /** List hidden folders too, or stop, and list the same folder again. */
  | { readonly kind: "toggle_hidden" }
  | { readonly kind: "scan" }
  /** Open the repository at `path` as a project. */
  | { readonly kind: "open_path"; readonly path: string }
  /** Clone what the URL field says, into the name field's folder if any. */
  | { readonly kind: "clone" };

/** A request that went out, so its answers can be told apart from others. */
export type PickerSent =
  | { readonly kind: "browse" }
  | { readonly kind: "scan" }
  | { readonly kind: "open"; readonly path: string }
  | { readonly kind: "clone"; readonly url: string };

/** One command for `main.ts` to send, and what to record once it has a seq. */
export interface PickerRequest {
  readonly name: string;
  readonly args?: Readonly<Record<string, unknown>>;
  readonly sent: PickerSent;
}

/** Said when the clone URL field is empty — the native picker's own words. */
export const CLONE_URL_MISSING = "Type the URL of the repository to clone.";

/** A browse of `path` (`null`: `$HOME`), honouring the hidden toggle. A blank
 * path is `$HOME`, as the native picker trims it. */
export function browseRequest(
  picker: PickerState,
  path: string | null,
): PickerRequest {
  const trimmed = path?.trim() ?? "";
  return {
    name: WORKER_COMMAND.BROWSE_DIR,
    args:
      trimmed === ""
        ? { show_hidden: picker.showHidden }
        : { path: trimmed, show_hidden: picker.showHidden },
    sent: { kind: "browse" },
  };
}

export function scanRequest(): PickerRequest {
  return { name: WORKER_COMMAND.SCAN_PROJECTS, sent: { kind: "scan" } };
}

export function openRequest(path: string): PickerRequest {
  return {
    name: WORKER_COMMAND.OPEN_PROJECT_PATH,
    args: { path },
    sent: { kind: "open", path },
  };
}

/** The clone the fields describe, or the reason there is none to send. */
export function cloneRequest(
  picker: PickerState,
): PickerRequest | { readonly error: string } {
  const url = picker.urlField.trim();
  if (url === "") {
    return { error: CLONE_URL_MISSING };
  }
  const name = picker.nameField.trim();
  return {
    name: WORKER_COMMAND.CLONE_PROJECT,
    args: name === "" ? { url } : { url, name },
    sent: { kind: "clone", url },
  };
}

// ---------------------------------------------------------------------------
// Requests sent
// ---------------------------------------------------------------------------

function remember(pending: PickerPending, seq: number): PickerPending {
  const mine = [...pending.mine, seq];
  return { ...pending, mine: mine.slice(Math.max(0, mine.length - SEQS_KEPT)) };
}

/** A request went out as `seq`. A new request clears the last error and
 * status: the person has moved on from both. */
export function pickerSent(
  picker: PickerState,
  sent: PickerSent,
  seq: number,
): PickerState {
  const pending = remember(picker.pending, seq);
  const base = { ...picker, error: null, status: null };
  switch (sent.kind) {
    case "browse":
      return { ...base, pending: { ...pending, browse: { seq, waiting: true } } };
    case "scan":
      return { ...base, pending: { ...pending, scan: { seq, waiting: true } } };
    case "open":
      return { ...base, pending: { ...pending, open: { seq, path: sent.path } } };
    case "clone":
      return {
        ...base,
        pending,
        clone: { cloneId: seq, url: sent.url, phase: "Starting", percent: null, done: null },
      };
  }
}

// ---------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------

/** A listing. Dropped unless it answers the latest browse. Moves the path
 * field to the folder now listed. */
export function pickerListing(
  picker: PickerState,
  listing: PickerListing,
): PickerState {
  const browse = picker.pending.browse;
  if (browse === null || browse.seq !== listing.seq || !browse.waiting) {
    return picker;
  }
  return {
    ...picker,
    pending: { ...picker.pending, browse: { seq: listing.seq, waiting: false } },
    home: listing.home,
    cwd: listing.path,
    parent: listing.parent,
    entries: listing.entries,
    truncated: listing.truncated,
    pathField: listing.path,
  };
}

/** A scan. Dropped unless it answers the latest scan; a refused scan is the
 * error line. */
export function pickerScan(
  picker: PickerState,
  scan: PickerScanAnswer,
): PickerState {
  const slot = picker.pending.scan;
  if (slot === null || slot.seq !== scan.seq || !slot.waiting) {
    return picker;
  }
  return {
    ...picker,
    pending: { ...picker.pending, scan: { seq: scan.seq, waiting: false } },
    error: scan.refusal ?? picker.error,
    scanned: { root: scan.root, rootExists: scan.rootExists, repos: scan.repos },
  };
}

/** Progress of this picker's clone; any other clone's is dropped. */
export function pickerCloneProgress(
  picker: PickerState,
  progress: CloneProgress,
): PickerState {
  const clone = picker.clone;
  if (clone === null || clone.cloneId !== progress.cloneId || clone.done !== null) {
    return picker;
  }
  return {
    ...picker,
    clone: { ...clone, phase: progress.phase, percent: progress.percent },
  };
}

/** How this picker's clone ended. On success the worker has opened it. */
export function pickerCloneResult(
  picker: PickerState,
  result: CloneResult,
): PickerState {
  const clone = picker.clone;
  if (clone === null || clone.cloneId !== result.cloneId || clone.done !== null) {
    return picker;
  }
  if (result.ok) {
    const path = result.path ?? "";
    return {
      ...picker,
      clone: { ...clone, done: { ok: true, path } },
      opened: { path, projectId: result.projectId },
    };
  }
  const why = result.error ?? "The clone failed.";
  return { ...picker, clone: { ...clone, done: { ok: false, why } }, error: why };
}

/** What a clone that a *closed* picker started says when it ends — the
 * status-bar notice the native client gives. */
export function cloneNotice(result: CloneResult): string {
  if (result.ok) {
    return result.path === null
      ? `Cloned ${result.url}.`
      : `Cloned ${result.url} into ${result.path}.`;
  }
  return result.error ?? "The clone failed.";
}

/**
 * The host's `ack` (or a refusing `error`) for one of the picker's requests.
 * A refusal is the error line and ends the request; an applied open is the
 * project opened. `owned` is whether the seq was the picker's at all.
 */
export function pickerAck(
  picker: PickerState,
  seq: number,
  outcome: "applied" | "rejected" | "ignored" | "read_only",
  detail: string | null,
): { readonly picker: PickerState; readonly owned: boolean } {
  const refused = outcome === "rejected" || outcome === "read_only";
  const why = detail ?? "The worker refused that.";
  const { pending } = picker;
  if (pending.browse !== null && pending.browse.seq === seq) {
    /** The listing itself is the answer; only a refusal ends it here. */
    return {
      owned: true,
      picker: refused
        ? { ...picker, pending: { ...pending, browse: null }, error: why }
        : picker,
    };
  }
  if (pending.scan !== null && pending.scan.seq === seq) {
    return {
      owned: true,
      picker: refused
        ? { ...picker, pending: { ...pending, scan: null }, error: why }
        : picker,
    };
  }
  if (pending.open !== null && pending.open.seq === seq) {
    const cleared = { ...pending, open: null };
    return {
      owned: true,
      picker: refused
        ? { ...picker, pending: cleared, error: why }
        : {
            ...picker,
            pending: cleared,
            status: detail,
            opened: { path: pending.open.path, projectId: null },
          },
    };
  }
  const clone = picker.clone;
  if (clone !== null && clone.cloneId === seq) {
    if (refused) {
      return {
        owned: true,
        picker: { ...picker, clone: { ...clone, done: { ok: false, why } }, error: why },
      };
    }
    return {
      owned: true,
      picker: detail === null ? picker : { ...picker, status: detail },
    };
  }
  /** A superseded request's ack: the picker's, and nothing to say. */
  return { owned: pending.mine.includes(seq), picker };
}

/** The panel's clone line: progress, or how it ended. `null` with no clone. */
export function cloneLine(clone: PickerClone | null): string | null {
  if (clone === null) {
    return null;
  }
  if (clone.done === null) {
    return clone.percent === null
      ? `Cloning ${clone.url} — ${clone.phase}`
      : `Cloning ${clone.url} — ${clone.phase} ${clone.percent}%`;
  }
  return clone.done.ok ? `Cloned into ${clone.done.path}.` : clone.done.why;
}

/** `/home/me/src/x` → `~/src/x`, for display only — the worker is always sent
 * the absolute path it gave us. */
export function tildePath(path: string, home: string): string {
  if (home === "" || home === "/") {
    return path;
  }
  if (path === home) {
    return "~";
  }
  const prefix = home.endsWith("/") ? home : `${home}/`;
  return path.startsWith(prefix) ? `~/${path.slice(prefix.length)}` : path;
}
