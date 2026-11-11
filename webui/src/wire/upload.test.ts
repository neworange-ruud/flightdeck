import { describe, expect, it } from "vitest";
import { CANCELLED } from "../state/upload";
import type { TransferResult } from "../state/upload";
import { putUpload, resultOf, UPLOAD_NETWORK_ERROR } from "./upload";

/**
 * `PUT /upload/<ticket>` (v8, R30), through a fake `XMLHttpRequest`: what is
 * sent, progress as it goes, the worker's JSON reply as the result, and Cancel.
 */

class FakeXhr {
  method = "";
  url = "";
  headers: Record<string, string> = {};
  body: unknown = undefined;
  status = 0;
  responseText = "";
  aborted = 0;
  readonly upload: { onprogress: ((event: ProgressEvent) => void) | null } = {
    onprogress: null,
  };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  ontimeout: (() => void) | null = null;
  onabort: (() => void) | null = null;

  open(method: string, url: string): void {
    this.method = method;
    this.url = url;
  }
  setRequestHeader(name: string, value: string): void {
    this.headers[name] = value;
  }
  send(body: unknown): void {
    this.body = body;
  }
  abort(): void {
    this.aborted += 1;
    this.onabort?.();
  }

  progress(loaded: number): void {
    this.upload.onprogress?.({ loaded } as ProgressEvent);
  }
  reply(status: number, body: unknown): void {
    this.status = status;
    this.responseText = typeof body === "string" ? body : JSON.stringify(body);
    this.onload?.();
  }
}

function start(ticket = "tk/1") {
  const xhr = new FakeXhr();
  const progress: number[] = [];
  const results: TransferResult[] = [];
  const body = new Blob(["hello"]);
  const transfer = putUpload({
    ticket,
    body,
    onProgress: (sent) => progress.push(sent),
    onDone: (result) => results.push(result),
    xhr: () => xhr as unknown as XMLHttpRequest,
  });
  return { xhr, progress, results, body, transfer };
}

describe("putUpload", () => {
  it("PUTs the file itself to the ticket's route, same origin", () => {
    const { xhr, body } = start("abc/def");
    expect(xhr.method).toBe("PUT");
    expect(xhr.url).toBe("/upload/abc%2Fdef");
    expect(xhr.body).toBe(body);
    expect(xhr.headers["Content-Type"]).toBe("application/octet-stream");
  });

  it("reports upload progress as it goes", () => {
    const { xhr, progress, results } = start();
    xhr.progress(2);
    xhr.progress(5);
    expect(progress).toEqual([2, 5]);
    expect(results).toEqual([]);
  });

  it("ends with the path the worker wrote", () => {
    const { xhr, results } = start();
    xhr.reply(200, { ok: true, path: "/home/me/dir/a.txt", bytes: 5 });
    expect(results).toEqual([{ ok: true, path: "/home/me/dir/a.txt" }]);
  });

  it("ends with the worker's own words on a refusal", () => {
    const { xhr, results } = start();
    xhr.reply(409, {
      ok: false,
      reason: "exists",
      message: "a.txt already exists in that folder.",
    });
    expect(results).toEqual([{ ok: false, why: "a.txt already exists in that folder." }]);
  });

  it("words a refusal that has none: an authentication refusal, a bare status", () => {
    expect(resultOf(401, JSON.stringify({ ok: false, screen: "code_entry" }))).toEqual({
      ok: false,
      why: "The worker refused this browser — pair it again and upload once more.",
    });
    expect(resultOf(507, JSON.stringify({ ok: false, reason: "no_space" }))).toEqual({
      ok: false,
      why: "The worker answered 507 (no_space); the file was not uploaded.",
    });
    expect(resultOf(502, "<html>bad gateway</html>")).toEqual({
      ok: false,
      why: "The worker answered 502; the file was not uploaded.",
    });
    /** A 200 that is not the worker's success is not one. */
    expect(resultOf(200, "{}").ok).toBe(false);
  });

  it("a broken connection is said as such", () => {
    const { xhr, results } = start();
    xhr.onerror?.();
    expect(results).toEqual([{ ok: false, why: UPLOAD_NETWORK_ERROR }]);
  });

  it("Cancel aborts the request and ends it once, as cancelled", () => {
    const { xhr, results, transfer } = start();
    transfer.abort();
    transfer.abort();
    expect(xhr.aborted).toBe(1);
    expect(results).toEqual([{ ok: false, why: CANCELLED }]);
    /** A reply racing the abort changes nothing. */
    xhr.reply(200, { ok: true, path: "p", bytes: 5 });
    expect(results).toHaveLength(1);
  });

  it("an abort after the end does nothing", () => {
    const { xhr, results, transfer } = start();
    xhr.reply(200, { ok: true, path: "p", bytes: 5 });
    transfer.abort();
    expect(xhr.aborted).toBe(0);
    expect(results).toEqual([{ ok: true, path: "p" }]);
  });
});
