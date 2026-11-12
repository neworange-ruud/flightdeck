/**
 * @vitest-environment jsdom
 *
 * Handing granted files to the browser (`GET /download/<ticket>`, v9): one
 * temporary `<a download>` per file, clicked once and removed, the first at
 * once and each next one a gap later — the clock injected, so the stagger is
 * read off rather than waited for.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DOWNLOAD_GAP_MS, DOWNLOAD_PATH, downloadUrl, startDownloads } from "./download";

interface Clicked {
  readonly href: string;
  readonly download: string;
  readonly attached: boolean;
}

let clicked: Clicked[];
let spy: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  document.body.replaceChildren();
  clicked = [];
  spy = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (
    this: HTMLAnchorElement,
  ) {
    clicked.push({
      href: this.getAttribute("href") ?? "",
      download: this.download,
      attached: this.isConnected,
    });
  });
});

afterEach(() => {
  spy.mockRestore();
});

/** A clock that runs nothing until told to. */
function manualClock() {
  const queued: { run: () => void; ms: number }[] = [];
  return {
    schedule: (run: () => void, ms: number) => {
      queued.push({ run, ms });
    },
    queued,
    runAll: () => {
      for (const { run } of queued.splice(0)) {
        run();
      }
    },
  };
}

describe("downloadUrl", () => {
  it("is the route and the ticket, escaped", () => {
    expect(DOWNLOAD_PATH).toBe("/download/");
    expect(downloadUrl("abc123")).toBe("/download/abc123");
    expect(downloadUrl("a/b c")).toBe("/download/a%2Fb%20c");
    expect(downloadUrl("t", "https://box:8443")).toBe("https://box:8443/download/t");
  });
});

describe("startDownloads", () => {
  it("clicks one anchor per file, named for the file, and leaves none behind", () => {
    const clock = manualClock();
    startDownloads({
      files: [
        { name: "notes.txt", ticket: "t-1" },
        { name: "data (1).bin", ticket: "t-2" },
        { name: "c", ticket: "t-3" },
      ],
      schedule: clock.schedule,
    });
    /** The first goes at once; the others wait their turn. */
    expect(clicked).toEqual([{ href: "/download/t-1", download: "notes.txt", attached: true }]);
    expect(clock.queued.map((q) => q.ms)).toEqual([DOWNLOAD_GAP_MS, 2 * DOWNLOAD_GAP_MS]);
    clock.runAll();
    expect(clicked.map((c) => [c.href, c.download])).toEqual([
      ["/download/t-1", "notes.txt"],
      ["/download/t-2", "data (1).bin"],
      ["/download/t-3", "c"],
    ]);
    expect(clicked.every((c) => c.attached)).toBe(true);
    expect(document.querySelectorAll("a[data-download]")).toHaveLength(0);
  });

  it("takes its gap and its document from the caller", () => {
    const clock = manualClock();
    const other = document.implementation.createHTMLDocument("other");
    startDownloads({
      files: [
        { name: "a", ticket: "1" },
        { name: "b", ticket: "2" },
      ],
      schedule: clock.schedule,
      gapMs: 50,
      document: other,
    });
    expect(clock.queued.map((q) => q.ms)).toEqual([50]);
    clock.runAll();
    expect(clicked).toHaveLength(2);
  });

  it("uses the real clock when none is given", () => {
    vi.useFakeTimers();
    try {
      startDownloads({
        files: [
          { name: "a", ticket: "1" },
          { name: "b", ticket: "2" },
        ],
      });
      expect(clicked).toHaveLength(1);
      vi.advanceTimersByTime(DOWNLOAD_GAP_MS - 1);
      expect(clicked).toHaveLength(1);
      vi.advanceTimersByTime(1);
      expect(clicked).toHaveLength(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does nothing for no files", () => {
    startDownloads({ files: [] });
    expect(clicked).toEqual([]);
  });
});
