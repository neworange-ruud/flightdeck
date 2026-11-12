import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { expect, test, type Browser, type Page } from "@playwright/test";

import {
  FIXTURE_REPO,
  pair,
  startWorker,
  stopWorker,
  tail,
  type WorkerHandle,
} from "./support/worker";

/**
 * The browser driving a **FlightDeck Worker** (`specs/WORKER_PRD.md`,
 * `remote-control-vjtb`) — end to end, against the real `flightdeck worker run`
 * serving this SPA from its own binary.
 *
 * Four things only a real worker and a real browser can prove together:
 *
 * 1. **W5/W15, the shell is sized to the window.** A worker has no pane of
 *    its own, so the browser measures its pane and the worker sizes the PTY to
 *    it — `stty size`, run *in* the worker shell, prints the grid the browser
 *    is showing, and a smaller window makes a smaller PTY.
 * 2. **W9, Open Project is the picker.** Browse `$HOME` into the projects
 *    root, see the fixture repository listed as a git repository, open it from
 *    the scan, and land on its tab.
 * 3. **W34, Upload files.** Two files chosen in the browser land in the
 *    selected project with their exact bytes, over the real `PUT /upload`;
 *    the same names again are a conflict, and Keep both writes `name (1).ext`
 *    beside them. A dropped file opens the same panel.
 *    **Download files** (v9): a file in the project, ticked on the Transfer
 *    files panel's Download tab, arrives as a real browser download over
 *    `GET /download`, byte for byte.
 * 4. **W17, takeover is terminal.** A second window takes the worker; the
 *    first shows who took it and does **not** take it back on its own — then
 *    "Take it back" does, and the other window is the one that stops.
 *
 * Its own worker, started here rather than in `globalSetup`: the chain and
 * narrow specs keep the TUI host they were written against, untouched.
 */

let worker: WorkerHandle | null = null;

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  test.setTimeout(20 * 60 * 1000);
  worker = await startWorker();
});

test.afterAll(async () => {
  await stopWorker(worker);
  worker = null;
});

function running(): WorkerHandle {
  if (worker === null) {
    throw new Error("the worker did not start");
  }
  return worker;
}

/** A fresh browser, paired with a fresh code from `flightdeck worker pair`,
 * attached as the worker's window. */
async function openWindow(browser: Browser): Promise<Page> {
  const handle = running();
  const context = await browser.newContext();
  const page = await context.newPage();
  const { code } = pair(handle);
  await page.goto(`${handle.baseURL}/#${code}`);
  await page
    .locator('.fd-frame[data-access="false"][data-host="worker"]')
    .waitFor({ timeout: 30_000 });
  await expect(page.locator(".fd-statusbar .fd-conn")).toContainText("connected");
  return page;
}

async function terminalText(page: Page): Promise<string> {
  return page.locator(".fd-pane .xterm-rows").innerText();
}

/** The last `rows cols` pair `stty size` printed. */
function lastSttySize(text: string): { rows: number; cols: number } | null {
  const matches = [...text.matchAll(/^\s*(\d+) (\d+)\s*$/gm)];
  const last = matches[matches.length - 1];
  return last === undefined
    ? null
    : { rows: Number(last[1]), cols: Number(last[2]) };
}

/**
 * The grid xterm.js is drawing, read off the page: one `.xterm-rows` child per
 * row, and the columns from the screen's width over one cell's — the cursor
 * is exactly one cell wide, whatever spacing the renderer gives a glyph.
 */
async function renderedGrid(page: Page): Promise<{ rows: number; cols: number }> {
  return page.evaluate(() => {
    const rowsEl = document.querySelector<HTMLElement>(".fd-pane .xterm-rows");
    const screen = document.querySelector<HTMLElement>(".fd-pane .xterm-screen");
    const cursor = document.querySelector<HTMLElement>(".fd-pane .xterm-rows .xterm-cursor");
    if (rowsEl === null || screen === null || cursor === null) {
      return { rows: 0, cols: 0 };
    }
    const cell = cursor.getBoundingClientRect().width;
    return {
      rows: rowsEl.children.length,
      cols: cell > 0 ? Math.round(screen.getBoundingClientRect().width / cell) : 0,
    };
  });
}

/** The grid once it has stopped moving: a window that just changed size is
 * re-measured and re-sent, and the PTY's answer arrives a round trip later. */
async function settledGrid(page: Page): Promise<{ rows: number; cols: number }> {
  let previous = "";
  let grid = await renderedGrid(page);
  await expect
    .poll(
      async () => {
        grid = await renderedGrid(page);
        const now = `${grid.rows}x${grid.cols}`;
        const stable = now === previous;
        previous = now;
        return stable;
      },
      { intervals: [500], timeout: 20_000 },
    )
    .toBe(true);
  return grid;
}

async function runStty(page: Page): Promise<{ rows: number; cols: number }> {
  const before = (await terminalText(page)).length;
  await page.keyboard.type("stty size");
  await page.keyboard.press("Enter");
  let size: { rows: number; cols: number } | null = null;
  await expect
    .poll(
      async () => {
        const text = await terminalText(page);
        size = text.length === before ? null : lastSttySize(text);
        return size;
      },
      { message: `stty size never answered:\n${tail(running().logPath)}` },
    )
    .not.toBeNull();
  if (size === null) {
    throw new Error("unreachable");
  }
  return size;
}

test("the worker shell is sized to the browser window (W5, W15)", async ({ browser }) => {
  const page = await openWindow(browser);
  /** A fresh worker opens on its empty workspace. */
  await expect(page.locator(".fd-workercard[data-kind='empty']")).toBeVisible();

  await page.getByRole("tab", { name: "Worker shell" }).click();
  await expect(page.locator(".fd-frame")).toHaveAttribute("data-worker-shell", "true");
  await page.locator(".fd-pane .xterm-screen").waitFor();
  await page.locator(".fd-pane .fd-stage").click();

  /** The grid grows to the pane: well past the 80×24 a PTY starts at, on a
   * 1600×1000 viewport. */
  await expect.poll(async () => (await renderedGrid(page)).cols).toBeGreaterThan(100);
  const grid = await settledGrid(page);
  const size = await runStty(page);
  expect(size.rows).toBe(grid.rows);
  expect(Math.abs(size.cols - grid.cols)).toBeLessThanOrEqual(1);

  /** A smaller window is a smaller PTY. */
  await page.setViewportSize({ width: 1100, height: 700 });
  await expect.poll(async () => (await renderedGrid(page)).rows).toBeLessThan(grid.rows);
  const smaller = await settledGrid(page);
  const resized = await runStty(page);
  expect(resized.rows).toBe(smaller.rows);
  expect(resized.cols).toBeLessThan(size.cols);
  expect(Math.abs(resized.cols - smaller.cols)).toBeLessThanOrEqual(1);

  await page.context().close();
});

test("Open Project browses and scans the worker, and opens a repository (W9)", async ({
  browser,
}) => {
  const page = await openWindow(browser);
  await page.locator('[data-action="open-project"]').first().click();
  const picker = page.locator(".fd-picker");
  await expect(picker).toBeVisible();

  /** Browse: `$HOME`, then into the projects root, where the fixture is a
   * git repository with an Open button. */
  const browse = picker.locator('[data-list="browse"]');
  await browse.locator('.fd-picker__item[data-name="flightdeck"] [data-action="enter"]').click();
  const listed = browse.locator(`.fd-picker__item[data-name="${FIXTURE_REPO}"]`);
  await expect(listed.locator('[data-action="open"]')).toBeVisible();

  /** Scan: the projects root holds the fixture; open it from there. */
  const scanned = picker.locator(
    `[data-list="scan"] .fd-picker__item[data-name="${FIXTURE_REPO}"]`,
  );
  await scanned.locator('[data-action="open"]').click();

  /** The picker closes and the window lands on the project's tab. */
  await expect(picker).toBeHidden();
  const tab = page.getByRole("tab", { name: FIXTURE_REPO });
  await expect(tab).toBeVisible();
  await expect(tab).toHaveAttribute("aria-selected", "true");
  await expect(page.locator(".fd-workercard[data-kind='empty']")).toBeHidden();

  await page.context().close();
});

/**
 * W34 / R30: Upload files, against the real `PUT /upload/<ticket>`. Runs after
 * Open Project, so the fixture repository is the selected project and so the
 * default folder.
 */
const NOTES = "hello from the browser\n";
/** Big enough to take more than one progress event; deterministic bytes. */
const DATA = Buffer.from(Array.from({ length: 300_000 }, (_, i) => (i * 31 + 7) % 251));

function onDisk(name: string): Buffer {
  return readFileSync(join(running().repoDir, name));
}

async function uploadInto(page: Page): Promise<void> {
  await page.locator("[data-upload-input]").setInputFiles([
    { name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from(NOTES) },
    { name: "data.bin", mimeType: "application/octet-stream", buffer: DATA },
  ]);
}

test("Upload files puts their bytes in the selected project (W34)", async ({ browser }) => {
  const page = await openWindow(browser);
  const tab = page.getByRole("tab", { name: FIXTURE_REPO });
  await expect(tab).toHaveAttribute("aria-selected", "true");
  await expect(page.locator('[data-action="transfer-files"]')).toBeVisible();

  await uploadInto(page);
  const panel = page.locator(".fd-upload");
  await expect(panel).toBeVisible();
  await expect(panel.locator('[data-list="files"] li')).toHaveCount(2);
  await expect(panel.locator(".fd-upload__dir")).toHaveValue(new RegExp(`/${FIXTURE_REPO}$`));

  await panel.locator('[data-action="upload"]').click();
  await expect(panel.locator("[data-summary]")).toHaveText(/^Uploaded 2 files to /, {
    timeout: 30_000,
  });
  expect(onDisk("notes.txt").toString("utf8")).toBe(NOTES);
  expect(onDisk("data.bin").equals(DATA)).toBe(true);

  /** The same names again: nothing is overwritten until asked, and Keep both
   * saves the new files beside the old under numbered names. */
  await panel.locator('[data-action="upload-more"]').click();
  await uploadInto(page);
  await panel.locator('[data-action="upload"]').click();
  const conflicts = panel.locator('[data-list="conflicts"] li');
  await expect(conflicts).toHaveCount(2);
  await expect(panel.locator('[data-list="conflicts"]')).toContainText("notes.txt");
  await panel.locator('[data-action="keep-both"]').click();
  await expect(panel.locator("[data-summary]")).toHaveText(/^Uploaded 2 files to /, {
    timeout: 30_000,
  });
  expect(onDisk("notes (1).txt").toString("utf8")).toBe(NOTES);
  expect(onDisk("data (1).bin").equals(DATA)).toBe(true);
  expect(onDisk("notes.txt").toString("utf8")).toBe(NOTES);

  await panel.locator('[data-action="close-done"]').click();
  await expect(panel).toBeHidden();
  await page.context().close();
});

test("dropping a file on the window opens the upload panel (R30)", async ({ browser }) => {
  const page = await openWindow(browser);
  /** A real `DragEvent` carrying a real `DataTransfer` with a `File` — what a
   * drop from the desktop delivers, short of the operating system. */
  await page.evaluate(() => {
    const data = new DataTransfer();
    data.items.add(new File(["dropped bytes\n"], "dropped.txt", { type: "text/plain" }));
    const frame = document.querySelector(".fd-frame");
    frame?.dispatchEvent(
      new DragEvent("dragenter", { bubbles: true, cancelable: true, dataTransfer: data }),
    );
    (window as unknown as { fdDrop: DataTransfer }).fdDrop = data;
  });
  await expect(page.locator(".fd-dropzone")).toBeVisible();
  await expect(page.locator(".fd-dropzone")).toContainText("Drop files to upload to");
  await page.evaluate(() => {
    const data = (window as unknown as { fdDrop: DataTransfer }).fdDrop;
    document
      .querySelector(".fd-frame")
      ?.dispatchEvent(
        new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: data }),
      );
  });
  await expect(page.locator(".fd-dropzone")).toBeHidden();
  const panel = page.locator(".fd-upload");
  await expect(panel).toBeVisible();
  await expect(panel.locator('[data-list="files"] li[data-name="dropped.txt"]')).toBeVisible();

  await panel.locator('[data-action="upload"]').click();
  await expect(panel.locator("[data-summary]")).toHaveText(/^Uploaded 1 file to /, {
    timeout: 30_000,
  });
  expect(onDisk("dropped.txt").toString("utf8")).toBe("dropped bytes\n");
  await page.keyboard.press("Escape");
  await expect(panel).toBeHidden();
  await page.context().close();
});

/**
 * v9: Download files, against the real `GET /download/<ticket>`. The file is
 * put in the fixture repository — the selected project, and so the Download
 * tab's starting folder — and must arrive as a browser download whose bytes
 * are the file's.
 */
const DOWNLOADED = Buffer.from(Array.from({ length: 200_000 }, (_, i) => (i * 17 + 3) % 253));

test("Download files hands a worker's file to the browser (v9)", async ({ browser }) => {
  writeFileSync(join(running().repoDir, "download-me.bin"), DOWNLOADED);
  const page = await openWindow(browser);
  await expect(page.getByRole("tab", { name: FIXTURE_REPO })).toHaveAttribute(
    "aria-selected",
    "true",
  );

  await page.locator('[data-action="transfer-files"]').click();
  const panel = page.locator(".fd-upload");
  await expect(panel).toBeVisible();
  await panel.locator('.fd-transfer__tab[data-tab="download"]').click();
  await expect(panel).toHaveAttribute("data-tab", "download");
  await expect(panel.locator(".fd-download__path")).toHaveText(new RegExp(`/${FIXTURE_REPO}$`));

  const row = panel.locator('[data-list="download"] li[data-kind="file"][data-name="download-me.bin"]');
  await expect(row).toBeVisible();
  await row.click();
  await expect(row).toHaveAttribute("aria-selected", "true");

  const [download] = await Promise.all([
    page.waitForEvent("download", { timeout: 30_000 }),
    panel.locator('.fd-download [data-action="download"]').click(),
  ]);
  expect(download.suggestedFilename()).toBe("download-me.bin");
  expect(await download.failure()).toBeNull();
  const saved = await download.path();
  expect(readFileSync(saved).equals(DOWNLOADED)).toBe(true);

  await expect(panel.locator(".fd-download [data-summary]")).toHaveText(
    "Sent 1 file to your browser's downloads.",
  );
  await panel.locator('.fd-download [data-action="close-done"]').click();
  await expect(panel).toBeHidden();
  await page.context().close();
});

test("a second window takes the worker over, and the first does not take it back (W17)", async ({
  browser,
}) => {
  const first = await openWindow(browser);
  const second = await openWindow(browser);

  const firstPanel = first.locator(".fd-takenover");
  await expect(firstPanel).toBeVisible();
  await expect(firstPanel).toContainText("Taken over by");
  await expect(first.locator(".fd-statusbar .fd-conn")).toContainText("taken over");

  /** Longer than the whole early retry ladder (250ms … 2s): a tab that
   * redialled on its own would have taken the worker back by now. */
  await first.waitForTimeout(5_000);
  await expect(firstPanel).toBeVisible();
  await expect(second.locator(".fd-takenover")).toBeHidden();
  await expect(second.locator(".fd-statusbar .fd-conn")).toContainText("connected");

  /** "Take it back" does — and then the *other* window is the one stopped. */
  await firstPanel.locator('[data-action="reclaim"]').click();
  await expect(firstPanel).toBeHidden();
  await expect(first.locator(".fd-statusbar .fd-conn")).toContainText("connected");
  await expect(second.locator(".fd-takenover")).toBeVisible();
  await second.waitForTimeout(3_000);
  await expect(second.locator(".fd-takenover")).toBeVisible();
  await expect(firstPanel).toBeHidden();

  await first.context().close();
  await second.context().close();
});
