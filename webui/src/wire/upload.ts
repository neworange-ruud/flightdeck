/**
 * The bytes of one upload: `PUT /upload/<ticket>` (v8, `specs/WEB_INTERFACE.md`
 * R30, `remote-control-usem`).
 *
 * The grant came over the socket (`upload_plan`); this sends one file's bytes
 * over HTTP on the same origin, so the session cookie rides along on its own.
 * `XMLHttpRequest` rather than `fetch`, because only it reports **upload**
 * progress, and it sets an exact `Content-Length` from the body — which the
 * worker requires, since the size is part of the grant.
 *
 * The worker answers JSON: `{ok: true, path, bytes}`, or `{ok: false, reason,
 * message}` with `message` a sentence for a person. An authentication refusal
 * carries `/auth/session`'s body instead, which has no `message`, so the
 * status words it here.
 *
 * Kept out of `state/` and `ui/` — the guard tests keep I/O and timing there
 * pure — and driven from `main.ts`. The XHR is injectable for tests.
 */

import type { TransferResult } from "../state/upload";
import { CANCELLED } from "../state/upload";

/** The route, as the worker serves it. */
export const UPLOAD_PATH = "/upload/";

/** Said when the connection broke under the transfer. */
export const UPLOAD_NETWORK_ERROR =
  "The connection to the worker dropped while sending; upload it again.";

/** One transfer under way. */
export interface UploadTransfer {
  /** Stop sending. `onDone` then reports `Cancelled.` (once). */
  abort(): void;
}

export interface PutUploadOptions {
  readonly ticket: string;
  readonly body: Blob;
  /** Bytes sent so far. */
  readonly onProgress: (sent: number) => void;
  /** How it ended. Called exactly once. */
  readonly onDone: (result: TransferResult) => void;
  /** Injected in tests. */
  readonly xhr?: () => XMLHttpRequest;
  /** The origin; same-origin (`""`) by default. */
  readonly base?: string;
}

/** Send `body` to `ticket`. Returns at once; the outcome arrives on
 * `onDone`. */
export function putUpload(options: PutUploadOptions): UploadTransfer {
  const request = options.xhr?.() ?? new XMLHttpRequest();
  let settled = false;
  const done = (result: TransferResult): void => {
    if (settled) {
      return;
    }
    settled = true;
    options.onDone(result);
  };

  request.open(
    "PUT",
    `${options.base ?? ""}${UPLOAD_PATH}${encodeURIComponent(options.ticket)}`,
  );
  request.setRequestHeader("Content-Type", "application/octet-stream");
  request.upload.onprogress = (event: ProgressEvent) => {
    options.onProgress(event.loaded);
  };
  request.onload = () => {
    done(resultOf(request.status, request.responseText));
  };
  request.onerror = () => done({ ok: false, why: UPLOAD_NETWORK_ERROR });
  request.ontimeout = () => done({ ok: false, why: UPLOAD_NETWORK_ERROR });
  request.onabort = () => done({ ok: false, why: CANCELLED });
  request.send(options.body);

  return {
    abort() {
      if (settled) {
        return;
      }
      request.abort();
      /** Some engines fire no `abort` for a request already finishing. */
      done({ ok: false, why: CANCELLED });
    },
  };
}

/**
 * The worker's reply as a transfer result: its `path` on success, else its
 * own `message` — or, from a reply that has none (an authentication refusal,
 * a proxy's page), the status in words.
 */
export function resultOf(status: number, text: string): TransferResult {
  let body: unknown = null;
  try {
    body = JSON.parse(text);
  } catch {
    body = null;
  }
  const reply = typeof body === "object" && body !== null ? (body as Record<string, unknown>) : {};
  if (status >= 200 && status < 300 && reply.ok === true && typeof reply.path === "string") {
    return { ok: true, path: reply.path };
  }
  if (typeof reply.message === "string" && reply.message !== "") {
    return { ok: false, why: reply.message };
  }
  return { ok: false, why: statusWords(status, reply.reason) };
}

function statusWords(status: number, reason: unknown): string {
  if (status === 401 || status === 403) {
    return "The worker refused this browser — pair it again and upload once more.";
  }
  if (status === 429) {
    return "Too many attempts; wait a moment and upload again.";
  }
  const code = typeof reason === "string" && reason !== "" ? ` (${reason})` : "";
  return status === 0
    ? UPLOAD_NETWORK_ERROR
    : `The worker answered ${status}${code}; the file was not uploaded.`;
}
