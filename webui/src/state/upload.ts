/**
 * Uploading files to a worker, as pure functions (`specs/WORKER_PRD.md` W34,
 * `specs/WEB_INTERFACE.md` R30, `remote-control-usem`).
 *
 * The person picks files on this machine and a folder on the worker, and
 * asks; the worker answers `begin_upload` with an `upload_plan` — a ticket per
 * file, the names already taken, or a refusal — and the files then go one at
 * a time over `PUT /upload/<ticket>` (`wire/upload.ts`). `UploadState` holds
 * what the panel shows and folds all of that in:
 *
 * ```text
 * choosing ──upload──▶ requested(seq) ──plan: tickets──▶ uploading ──▶ done
 *    ▲                    │   │                              │
 *    │    refusal / error │   └─plan: conflicts─▶ asking ────┤ replace / keep both
 *    └────────────────────┘                        │  cancel │  (re-request)
 *                       cancelled ◀── cancel (any phase) ────┘
 * ```
 *
 * A port of the native client's `UploadPanel` (`src/web/client/upload.rs`),
 * rule for rule, so a browser and FlightDeck Desktop upload the same way:
 *
 * - **It sends nothing and touches no file.** `uploadRequest` says what to
 *   send; `main.ts` sends it, reports the seq back (`uploadRequested`),
 *   starts the transfer `nextUploadJob` names, and feeds back the plan, the
 *   acks, and the transfer's progress and end.
 * - **An answer to anything but the latest request is dropped.**
 * - **One file per name**, and folders are refused with a note.
 *
 * A picked file carries its `Blob` as an opaque handle: this module never
 * reads it, it only hands it back in the job `main.ts` sends.
 */

import type { Project, Session } from "./model";
import type { AppState } from "./types";
import { WORKER_COMMAND } from "./worker";

export { UPLOAD_FILES_COMMAND, UPLOAD_MIN_VERSION, uploadAvailable } from "./worker";

/** How many of its own seqs the panel remembers, so a superseded request's
 * ack is still recognised as the panel's. */
const SEQS_KEPT = 16;

/** Said when a picked item is a folder. */
export const FOLDER_REFUSED = "Folders cannot be uploaded; choose the files inside.";

/** Said when an upload is asked for with no files. */
export const NO_FILES = "Choose at least one file to upload.";

/** Said when the link dropped while the worker was being asked. */
export const LINK_LOST = "The connection to the worker dropped; upload again.";

/** Said when the request could not be sent at all (`worker::NOT_SENT`). */
export const NOT_SENT = "Not connected to the worker — nothing was sent.";

/** Said for a file the person cancelled before it was sent. */
export const CANCELLED = "Cancelled.";

/** Said when the worker granted none of the files. */
export const GRANTED_NOTHING = "The worker granted nothing to upload.";

/** Said for a refusal that came without words. */
export const REFUSED = "The worker refused the upload.";

/** A file on this machine, as picked. */
export interface LocalFile {
  /** Its name: what it is called on the worker unless that is taken. */
  readonly name: string;
  /** Its size in bytes when picked. */
  readonly size: number;
  /** The bytes, as the browser handed them over. Never read here. */
  readonly source: Blob;
}

/** One thing the person picked or dropped. */
export type Picked =
  | { readonly kind: "file"; readonly file: LocalFile }
  /** A folder: refused with `FOLDER_REFUSED`. */
  | { readonly kind: "folder"; readonly name: string }
  /** Unreadable, with why. */
  | { readonly kind: "unreadable"; readonly name: string; readonly why: string };

/** What to do about taken names (`protocol::OnConflict`). */
export type OnConflict = "ask" | "replace" | "keep_both";

/** The person's answer to taken names. */
export type Resolution = "replace" | "keep_both" | "cancel";

/** What to send: a `begin_upload`'s arguments. */
export interface UploadRequest {
  readonly dir: string;
  readonly files: readonly { readonly name: string; readonly size: number }[];
  readonly onConflict: OnConflict;
}

/** `ServerMsg::UploadPlan`, as the panel takes it. */
export interface UploadPlan {
  readonly seq: number;
  /** The target folder, canonical and absolute on the worker. */
  readonly dir: string;
  readonly files: readonly UploadPlanFile[];
  readonly conflicts: boolean;
  readonly refusal: string | null;
}

export interface UploadPlanFile {
  readonly name: string;
  readonly finalName: string;
  readonly ticket: string | null;
  readonly exists: boolean;
}

/** How one file of a granted upload stands. */
export type ItemState =
  | { readonly kind: "waiting" }
  | { readonly kind: "sending" }
  /** On the worker, at this path. */
  | { readonly kind: "written"; readonly path: string }
  | { readonly kind: "failed"; readonly why: string };

/** One file of a granted upload. */
export interface UploadItem {
  /** The name asked for. */
  readonly name: string;
  /** The name the worker gives it. */
  readonly finalName: string;
  readonly source: Blob;
  readonly ticket: string;
  /** Bytes sent so far. */
  readonly sent: number;
  readonly total: number;
  readonly state: ItemState;
}

/** One transfer to start: `PUT` `source` to `ticket`. */
export interface UploadJob {
  /** Which item, for `uploadProgress` and `uploadFinished`. */
  readonly index: number;
  readonly source: Blob;
  readonly ticket: string;
  /** Its exact size, as granted. */
  readonly size: number;
}

export type UploadPhase =
  /** Picking files and a folder. */
  | { readonly kind: "choosing" }
  /** A `begin_upload` is out as `seq`. */
  | { readonly kind: "requested"; readonly seq: number; readonly onConflict: OnConflict }
  /** Names are taken; the person chooses. */
  | { readonly kind: "asking"; readonly dir: string; readonly conflicts: readonly string[] }
  /** The files are going out, one at a time. */
  | {
      readonly kind: "uploading";
      readonly dir: string;
      readonly items: readonly UploadItem[];
      /** The one being sent, if any. */
      readonly current: number | null;
    }
  /** Every file has been sent or has failed. */
  | {
      readonly kind: "done";
      readonly dir: string;
      /** The files written, by final name. */
      readonly written: readonly string[];
      /** The ones that were not, with why. */
      readonly failed: readonly { readonly name: string; readonly why: string }[];
    }
  | { readonly kind: "cancelled" };

export interface UploadState {
  /** The target folder on the worker, as typed. */
  readonly dir: string;
  /** The files picked, in order, one per name. */
  readonly files: readonly LocalFile[];
  readonly phase: UploadPhase;
  /** The last thing worth saying: a refusal, a skipped pick. */
  readonly note: string | null;
  /** Every seq the panel sent lately, newest last. */
  readonly mine: readonly number[];
}

/** A panel uploading into `dir`, with nothing picked yet. */
export function newUpload(dir: string): UploadState {
  return { dir, files: [], phase: { kind: "choosing" }, note: null, mine: [] };
}

/**
 * The folder an upload goes to by default: the selected session's worktree,
 * else the selected project's root, else the worker's `$HOME`, else `~`
 * (`upload::default_dir`).
 */
export function defaultUploadDir(state: AppState): string {
  const selection = state.selection;
  const project: Project | null =
    selection === null
      ? null
      : (state.projects.find((p) => p.id === selection.projectId) ?? null);
  const session: Session | null =
    project === null || selection === null
      ? null
      : (project.sessions.find((s) => s.id === selection.sessionId) ?? null);
  const candidates = [session?.worktreePath, project?.root, state.worker?.home];
  for (const candidate of candidates) {
    if (candidate !== undefined && candidate !== null && candidate !== "") {
      return candidate;
    }
  }
  return "~";
}

export function isChoosing(upload: UploadState): boolean {
  return upload.phase.kind === "choosing";
}

export function isUploading(upload: UploadState): boolean {
  return upload.phase.kind === "uploading";
}

/** The total size of the files picked. */
export function totalBytes(upload: UploadState): number {
  return upload.files.reduce((sum, file) => sum + file.size, 0);
}

/** `{sent, total}` bytes over every granted file, while uploading. */
export function progressBytes(
  upload: UploadState,
): { readonly sent: number; readonly total: number } | null {
  if (upload.phase.kind !== "uploading") {
    return null;
  }
  return upload.phase.items.reduce(
    (acc, item) => ({ sent: acc.sent + item.sent, total: acc.total + item.total }),
    { sent: 0, total: 0 },
  );
}

// ---------------------------------------------------------------------------
// Choosing
// ---------------------------------------------------------------------------

/**
 * Add what the person picked. A name already in the list is skipped (one file
 * per name), a folder is refused; either is said in the note. Ignored outside
 * `choosing`.
 */
export function uploadAdd(upload: UploadState, picked: readonly Picked[]): UploadState {
  if (!isChoosing(upload) || picked.length === 0) {
    return upload;
  }
  const files = [...upload.files];
  const notes: string[] = [];
  for (const pick of picked) {
    switch (pick.kind) {
      case "file":
        if (files.some((f) => f.name === pick.file.name)) {
          notes.push(`${pick.file.name} is already in the list.`);
        } else {
          files.push(pick.file);
        }
        break;
      case "folder":
        if (!notes.includes(FOLDER_REFUSED)) {
          notes.push(FOLDER_REFUSED);
        }
        break;
      case "unreadable":
        notes.push(`Could not read ${pick.name}: ${pick.why}`);
        break;
    }
  }
  return { ...upload, files, note: notes.length === 0 ? null : notes.join(" ") };
}

/** Take the file at `index` out of the list. */
export function uploadRemove(upload: UploadState, index: number): UploadState {
  if (!isChoosing(upload) || index < 0 || index >= upload.files.length) {
    return upload;
  }
  return {
    ...upload,
    files: upload.files.filter((_, i) => i !== index),
    note: null,
  };
}

/** Upload into `dir` instead. */
export function uploadSetDir(upload: UploadState, dir: string): UploadState {
  if (!isChoosing(upload) || upload.dir === dir) {
    return upload;
  }
  return { ...upload, dir };
}

// ---------------------------------------------------------------------------
// Asking the worker
// ---------------------------------------------------------------------------

/**
 * What to send for Upload: the list, the folder, and `ask` for taken names —
 * or, when there is nothing to send, the note saying why.
 */
export function uploadRequest(
  upload: UploadState,
): UploadRequest | { readonly note: string } | null {
  if (!isChoosing(upload)) {
    return null;
  }
  if (upload.files.length === 0) {
    return { note: NO_FILES };
  }
  return requestWith(upload, "ask");
}

function requestWith(upload: UploadState, onConflict: OnConflict): UploadRequest {
  return {
    dir: upload.dir.trim(),
    files: upload.files.map((f) => ({ name: f.name, size: f.size })),
    onConflict,
  };
}

/** A request as the worker command it is: `begin_upload` and its args. */
export function beginUploadCommand(request: UploadRequest): {
  readonly name: string;
  readonly args: Readonly<Record<string, unknown>>;
} {
  return {
    name: WORKER_COMMAND.BEGIN_UPLOAD,
    args: {
      dir: request.dir,
      files: request.files.map((f) => ({ name: f.name, size: f.size })),
      on_conflict: request.onConflict,
    },
  };
}

/** The request went out as `seq` (`null`: it could not be sent). */
export function uploadRequested(
  upload: UploadState,
  seq: number | null,
  onConflict: OnConflict,
): UploadState {
  if (seq === null) {
    return { ...upload, phase: { kind: "choosing" }, note: NOT_SENT };
  }
  const mine = [...upload.mine, seq];
  return {
    ...upload,
    mine: mine.slice(Math.max(0, mine.length - SEQS_KEPT)),
    phase: { kind: "requested", seq, onConflict },
    note: null,
  };
}

/**
 * The person's answer to taken names: a request to send again with that
 * choice, or (`cancel`) back to the list.
 */
export function uploadResolve(
  upload: UploadState,
  resolution: Resolution,
): { readonly upload: UploadState; readonly request: UploadRequest | null } {
  if (upload.phase.kind !== "asking") {
    return { upload, request: null };
  }
  switch (resolution) {
    case "cancel":
      return {
        upload: { ...upload, phase: { kind: "choosing" }, note: null },
        request: null,
      };
    case "replace":
      return { upload, request: requestWith(upload, "replace") };
    case "keep_both":
      return { upload, request: requestWith(upload, "keep_both") };
  }
}

// ---------------------------------------------------------------------------
// The worker's answers
// ---------------------------------------------------------------------------

/** The worker's plan. Dropped unless it answers the request out now. */
export function uploadPlan(upload: UploadState, plan: UploadPlan): UploadState {
  const phase = upload.phase;
  if (phase.kind !== "requested" || plan.seq !== phase.seq) {
    return upload;
  }
  if (plan.refusal !== null) {
    return { ...upload, phase: { kind: "choosing" }, note: plan.refusal };
  }
  if (plan.conflicts) {
    return {
      ...upload,
      phase: {
        kind: "asking",
        dir: plan.dir,
        conflicts: plan.files.filter((f) => f.exists).map((f) => f.name),
      },
    };
  }
  const items: UploadItem[] = [];
  for (const row of plan.files) {
    const file = upload.files.find((f) => f.name === row.name);
    if (file === undefined || row.ticket === null) {
      continue;
    }
    items.push({
      name: row.name,
      finalName: row.finalName,
      source: file.source,
      ticket: row.ticket,
      sent: 0,
      total: file.size,
      state: { kind: "waiting" },
    });
  }
  if (items.length === 0) {
    return { ...upload, phase: { kind: "choosing" }, note: GRANTED_NOTHING };
  }
  return {
    ...upload,
    phase: { kind: "uploading", dir: plan.dir, items, current: null },
    note: null,
  };
}

/**
 * An ack (or a refusing error) for one of the panel's requests: a refusal
 * ends the request with its reason. `owned` is whether the seq was the
 * panel's at all.
 */
export function uploadAck(
  upload: UploadState,
  seq: number,
  outcome: "applied" | "rejected" | "ignored" | "read_only",
  detail: string | null,
): { readonly upload: UploadState; readonly owned: boolean } {
  if (!upload.mine.includes(seq)) {
    return { upload, owned: false };
  }
  const phase = upload.phase;
  const current = phase.kind === "requested" && phase.seq === seq;
  const refused = outcome === "rejected" || outcome === "read_only";
  if (current && refused) {
    return {
      owned: true,
      upload: { ...upload, phase: { kind: "choosing" }, note: detail ?? REFUSED },
    };
  }
  return { upload, owned: true };
}

/**
 * The link dropped: a request still out will not be answered (commands are
 * not carried across a reconnect). A transfer carries on — its ticket does not
 * need the socket.
 */
export function uploadLinkLost(upload: UploadState): UploadState {
  if (upload.phase.kind !== "requested") {
    return upload;
  }
  return { ...upload, phase: { kind: "choosing" }, note: LINK_LOST };
}

// ---------------------------------------------------------------------------
// The transfer
// ---------------------------------------------------------------------------

/** The next file to send, when none is being sent. `null` when one is under
 * way, or none is left. Pure: `uploadStarted` marks it. */
export function nextUploadJob(upload: UploadState): UploadJob | null {
  const phase = upload.phase;
  if (phase.kind !== "uploading" || phase.current !== null) {
    return null;
  }
  const index = phase.items.findIndex((i) => i.state.kind === "waiting");
  const item = phase.items[index];
  if (index < 0 || item === undefined) {
    return null;
  }
  return { index, source: item.source, ticket: item.ticket, size: item.total };
}

/** Item `index`'s transfer started: it is the one sending. */
export function uploadStarted(upload: UploadState, index: number): UploadState {
  const phase = upload.phase;
  if (phase.kind !== "uploading" || phase.current !== null) {
    return upload;
  }
  const item = phase.items[index];
  if (item === undefined || item.state.kind !== "waiting") {
    return upload;
  }
  return {
    ...upload,
    phase: {
      ...phase,
      current: index,
      items: replaceAt(phase.items, index, { ...item, state: { kind: "sending" } }),
    },
  };
}

/** `sent` bytes of item `index` are out. */
export function uploadProgress(
  upload: UploadState,
  index: number,
  sent: number,
): UploadState {
  const phase = upload.phase;
  if (phase.kind !== "uploading") {
    return upload;
  }
  const item = phase.items[index];
  if (item === undefined || item.state.kind !== "sending" || item.sent === sent) {
    return upload;
  }
  return {
    ...upload,
    phase: {
      ...phase,
      items: replaceAt(phase.items, index, {
        ...item,
        sent: Math.min(Math.max(0, sent), item.total),
      }),
    },
  };
}

/** How one transfer ended. */
export type TransferResult =
  | { readonly ok: true; readonly path: string }
  | { readonly ok: false; readonly why: string };

/** Item `index` ended. The last one ends the upload (`done`). */
export function uploadFinished(
  upload: UploadState,
  index: number,
  result: TransferResult,
): UploadState {
  const phase = upload.phase;
  if (phase.kind !== "uploading") {
    return upload;
  }
  const item = phase.items[index];
  if (item === undefined || item.state.kind !== "sending") {
    return upload;
  }
  const ended: UploadItem = result.ok
    ? { ...item, sent: item.total, state: { kind: "written", path: result.path } }
    : { ...item, state: { kind: "failed", why: result.why } };
  const items = replaceAt(phase.items, index, ended);
  const current = phase.current === index ? null : phase.current;
  if (items.some((i) => i.state.kind === "waiting" || i.state.kind === "sending")) {
    return { ...upload, phase: { ...phase, items, current } };
  }
  const written: string[] = [];
  const failed: { name: string; why: string }[] = [];
  for (const i of items) {
    if (i.state.kind === "written") {
      written.push(i.finalName);
    } else if (i.state.kind === "failed") {
      failed.push({ name: i.finalName, why: i.state.why });
    }
  }
  return { ...upload, phase: { kind: "done", dir: phase.dir, written, failed } };
}

/**
 * Stop. `sending` is whether a transfer was under way, which the caller then
 * aborts. What was written stays written.
 */
export function uploadCancel(
  upload: UploadState,
): { readonly upload: UploadState; readonly sending: boolean } {
  const phase = upload.phase;
  const sending = phase.kind === "uploading" && phase.current !== null;
  return {
    upload: { ...upload, phase: { kind: "cancelled" }, note: null },
    sending,
  };
}

/** Back to an empty list (after done or cancelled), same folder. */
export function uploadStartOver(upload: UploadState): UploadState {
  return { ...upload, files: [], phase: { kind: "choosing" }, note: null };
}

function replaceAt<T>(list: readonly T[], index: number, value: T): T[] {
  return list.map((item, i) => (i === index ? value : item));
}

// ---------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------

/** A file's size as people read one: binary units, one decimal under 10. */
export function humanSize(bytes: number): string {
  const units = ["KB", "MB", "GB", "TB"];
  if (bytes < 1024) {
    return bytes === 1 ? "1 byte" : `${bytes} bytes`;
  }
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const shown = value < 10 ? value.toFixed(1) : String(Math.round(value));
  return `${shown} ${units[unit]}`;
}

/** What the drop overlay calls the worker: the name it advertises, else the
 * host this tab reached, without a port — else just "the worker". */
export function workerName(state: AppState): string {
  const named = state.worker?.status.advertised ?? null;
  const name = named !== null && named !== "" ? named : state.host;
  const bare = name.replace(/:\d+$/, "");
  return bare === "" ? "the worker" : bare;
}

/** The Done panel's headline: how many went, and where. */
export function doneLine(written: number, failed: number, dir: string): string {
  const files = (n: number): string => (n === 1 ? "1 file" : `${n} files`);
  if (failed === 0) {
    return `Uploaded ${files(written)} to ${dir}.`;
  }
  if (written === 0) {
    return failed === 1 ? `The file was not uploaded.` : `None of the ${failed} files were uploaded.`;
  }
  return `Uploaded ${files(written)} to ${dir}; ${failed} failed.`;
}
