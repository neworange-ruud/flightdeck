/**
 * The bytes of a download: the browser fetches `GET /download/<ticket>` itself
 * (v9, `remote-control-pz6d`).
 *
 * The grant came over the socket (`download_plan`); the file comes over HTTP
 * on the same origin, so the session cookie rides along on its own. Unlike an
 * upload, nothing here streams the bytes through JavaScript: each file is a
 * temporary `<a href download="name">` clicked once, which hands it to the
 * browser's own download manager — progress, the save location, a resumed
 * failure are all the browser's. The worker answers `200` with
 * `Content-Disposition: attachment`, so even an engine that ignored the
 * `download` attribute would save rather than navigate.
 *
 * **One file at a time, a little apart.** A page that starts several
 * downloads at once is what Chrome's "download multiple files" prompt exists
 * for; started together, the ones after the first can be dropped while the
 * prompt is up. Staggered, the prompt appears once and every file waits for
 * the answer. Every ticket lapses two minutes after the plan, so the stagger
 * stays short.
 *
 * Kept out of `state/` and `ui/` — the guard tests keep I/O and timing there
 * pure — and driven from `main.ts`. The document and the clock are injectable
 * for tests.
 */

/** The route, as the worker serves it. */
export const DOWNLOAD_PATH = "/download/";

/** The gap between two files' downloads. */
export const DOWNLOAD_GAP_MS = 400;

/** One granted file: its ticket, and the name to save it as. */
export interface DownloadHandoff {
  readonly name: string;
  readonly ticket: string;
}

export interface StartDownloadsOptions {
  readonly files: readonly DownloadHandoff[];
  /** The origin; same-origin (`""`) by default. */
  readonly base?: string;
  /** Where the temporary anchors go. Injected in tests. */
  readonly document?: Document;
  /** Run `run` after `ms`. Injected in tests; `setTimeout` otherwise. */
  readonly schedule?: (run: () => void, ms: number) => void;
  readonly gapMs?: number;
}

/** The URL one ticket is fetched from. */
export function downloadUrl(ticket: string, base = ""): string {
  return `${base}${DOWNLOAD_PATH}${encodeURIComponent(ticket)}`;
}

/**
 * Hand every file to the browser: the first now, each next one `gapMs` after
 * the one before. Returns at once.
 */
export function startDownloads(options: StartDownloadsOptions): void {
  const doc = options.document ?? document;
  const schedule =
    options.schedule ??
    ((run: () => void, ms: number): void => {
      setTimeout(run, ms);
    });
  const gap = options.gapMs ?? DOWNLOAD_GAP_MS;
  options.files.forEach((file, index) => {
    const start = (): void => clickDownload(doc, downloadUrl(file.ticket, options.base), file.name);
    if (index === 0) {
      start();
    } else {
      schedule(start, index * gap);
    }
  });
}

/** One download: a hidden anchor, clicked, then taken away again. */
function clickDownload(doc: Document, href: string, name: string): void {
  const anchor = doc.createElement("a");
  anchor.href = href;
  anchor.download = name;
  anchor.rel = "noopener";
  anchor.hidden = true;
  anchor.setAttribute("data-download", name);
  (doc.body ?? doc.documentElement).append(anchor);
  anchor.click();
  anchor.remove();
}
