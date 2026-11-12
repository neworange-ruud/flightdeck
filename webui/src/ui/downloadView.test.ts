/**
 * @vitest-environment jsdom
 *
 * The Transfer files panel's Download tab, rendered (v9): the tabs, the
 * folder and its files, the keyboard on the list, and the tab through its
 * phases. Everything is driven through the store; `main.ts`'s sending is out
 * of scope, so what a click *means* is read off the dispatched actions.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { MULTIPLE_HINT, NO_SELECTION } from "../state/download";
import type { PickerListing } from "../state/picker";
import type { AppAction } from "../state/types";
import { snapshotFromWire } from "../wire/adapt";
import type { WireSnapshot } from "../wire/frames";
import { WORKER_HOME, workerSnapshotWire } from "../wire/workerFixture";
import { createApp } from "./app";

const DIR = `${WORKER_HOME}/dir`;

function render(snapshot: WireSnapshot = workerSnapshotWire()) {
  const dispatched: AppAction[] = [];
  const app = createApp({
    mount: (container, _geometry, terminalId) => {
      container.append(`[${terminalId}]`);
      return { dispose: () => undefined, resize: () => undefined };
    },
    onDispatch: (action) => dispatched.push(action),
  });
  document.body.append(app.el);
  app.store.dispatch({ type: "snapshot/received", snapshot: snapshotFromWire(snapshot) });
  app.store.dispatch({ type: "connection/changed", status: "connected" });
  const maybe = (selector: string): HTMLElement | null =>
    app.el.querySelector<HTMLElement>(selector);
  const q = (selector: string): HTMLElement => {
    const found = maybe(selector);
    if (found === null) {
      throw new Error(`no element matched ${selector}`);
    }
    return found;
  };
  const rows = (): HTMLElement[] => [
    ...app.el.querySelectorAll<HTMLElement>('[data-list="download"] li'),
  ];
  return { app, dispatched, q, maybe, rows };
}

function listing(seq: number, path = DIR): PickerListing {
  return {
    seq,
    path,
    parent: path === WORKER_HOME ? null : WORKER_HOME,
    home: WORKER_HOME,
    entries: [{ name: "sub", path: `${path}/sub`, isGitRepo: false, isOpenProject: false }],
    files: [
      { name: "notes.txt", path: `${path}/notes.txt`, size: 23 },
      { name: "data.bin", path: `${path}/data.bin`, size: 2048 },
    ],
    truncated: false,
  };
}

/** The panel open on Download, `DIR` listed. */
function onDownload(h: ReturnType<typeof render>): void {
  h.app.store.dispatch({ type: "transfer/open", tab: "download" });
  h.app.store.dispatch({ type: "download/browsing", seq: 1 });
  h.app.store.dispatch({ type: "picker/listing", listing: listing(1) });
}

function visible(element: HTMLElement | null): boolean {
  return element !== null && !element.hidden;
}

function keyOn(target: HTMLElement, name: string): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { key: name, bubbles: true, cancelable: true });
  target.dispatchEvent(event);
  return event;
}

beforeEach(() => {
  document.body.replaceChildren();
});

describe("the tabs", () => {
  it("shows Upload and Download, and switches between them", () => {
    const h = render();
    h.app.store.dispatch({ type: "transfer/open", tab: null });
    const upload = h.q('.fd-transfer__tab[data-tab="upload"]');
    const download = h.q('.fd-transfer__tab[data-tab="download"]');
    expect(visible(h.q(".fd-transfer__tabs"))).toBe(true);
    expect(upload.getAttribute("aria-selected")).toBe("true");
    expect(visible(h.q("#fd-transfer-upload"))).toBe(true);
    expect(visible(h.q("#fd-transfer-download"))).toBe(false);

    download.click();
    expect(h.dispatched).toContainEqual({ type: "transfer/open", tab: "download" });
    expect(download.getAttribute("aria-selected")).toBe("true");
    expect(upload.getAttribute("aria-selected")).toBe("false");
    expect(visible(h.q("#fd-transfer-upload"))).toBe(false);
    expect(visible(h.q("#fd-transfer-download"))).toBe(true);
    expect(h.app.el.getAttribute("data-download")).toBe("browsing");

    /** ←→ on the tablist walk the tabs. */
    keyOn(h.q(".fd-transfer__tabs"), "ArrowRight");
    expect(h.app.store.getState().transferTab).toBe("upload");
  });

  it("reopens on the tab last used", () => {
    const h = render();
    onDownload(h);
    h.app.store.dispatch({ type: "transfer/close" });
    expect(visible(h.q(".fd-upload"))).toBe(false);
    h.q('[data-action="transfer-files"]').click();
    expect(h.q(".fd-upload").getAttribute("data-tab")).toBe("download");
  });

  it("will not switch while files are uploading", () => {
    const h = render();
    h.app.store.dispatch({
      type: "upload/open",
      picked: [{ kind: "file", file: { name: "a.txt", size: 3, source: new Blob(["abc"]) } }],
    });
    h.app.store.dispatch({ type: "upload/requested", seq: 4, onConflict: "ask" });
    h.app.store.dispatch({
      type: "upload/plan",
      plan: {
        seq: 4,
        dir: WORKER_HOME,
        files: [{ name: "a.txt", finalName: "a.txt", ticket: "t", exists: false }],
        conflicts: false,
        refusal: null,
      },
    });
    const download = h.q('.fd-transfer__tab[data-tab="download"]') as HTMLButtonElement;
    expect(download.disabled).toBe(true);
    h.app.store.dispatch({ type: "transfer/open", tab: "download" });
    expect(h.q(".fd-upload").getAttribute("data-tab")).toBe("upload");
  });
});

describe("the Download tab", () => {
  it("shows the folder in tilde form, its folders, and its files with sizes", () => {
    const h = render();
    onDownload(h);
    expect(h.q(".fd-download__path").textContent).toBe("~/dir");
    const rows = h.rows();
    expect(rows.map((r) => [r.getAttribute("data-kind"), r.getAttribute("data-name")])).toEqual([
      ["folder", "sub"],
      ["file", "notes.txt"],
      ["file", "data.bin"],
    ]);
    expect(rows[0]?.textContent).toContain("▸");
    expect(rows[0]?.textContent).toContain("sub/");
    expect(rows[1]?.querySelector('input[type="checkbox"]')).not.toBeNull();
    expect(rows[1]?.textContent).toContain("23 bytes");
    expect(rows[2]?.textContent).toContain("2.0 KB");
    expect(h.q('[data-list="download"]').getAttribute("role")).toBe("listbox");
    /** Nothing ticked: Download waits. */
    expect(h.q('.fd-download [data-action="download"]').hasAttribute("disabled")).toBe(true);
  });

  it("says Listing… until the first listing, and nothing is listed from a stale one", () => {
    const h = render();
    h.app.store.dispatch({ type: "transfer/open", tab: "download" });
    h.app.store.dispatch({ type: "download/browsing", seq: 2 });
    expect(h.q(".fd-download .fd-picker__busy").textContent).toBe("Listing…");
    expect(h.q(".fd-download__path").textContent).toBe("~");
    h.app.store.dispatch({ type: "picker/listing", listing: listing(1) });
    expect(h.rows()).toHaveLength(0);
    h.app.store.dispatch({ type: "picker/listing", listing: listing(2) });
    expect(h.rows()).toHaveLength(3);
    expect(h.q(".fd-download .fd-picker__busy").textContent).toBe("");
  });

  it("Up, Home and Show hidden are intents, and Up waits at home", () => {
    const h = render();
    onDownload(h);
    h.q('.fd-download [data-action="up"]').click();
    h.q('.fd-download [data-action="home"]').click();
    h.q('.fd-download [data-action="toggle-hidden"]').click();
    expect(h.dispatched).toContainEqual({ type: "download/intent", intent: { kind: "up" } });
    expect(h.dispatched).toContainEqual({ type: "download/intent", intent: { kind: "home" } });
    expect(h.dispatched).toContainEqual({
      type: "download/intent",
      intent: { kind: "toggle_hidden" },
    });
    expect(h.q('.fd-download [data-action="toggle-hidden"]').textContent).toBe("Hide hidden");
    h.app.store.dispatch({ type: "download/browsing", seq: 3 });
    h.app.store.dispatch({ type: "picker/listing", listing: listing(3, WORKER_HOME) });
    expect(h.q('.fd-download [data-action="up"]').hasAttribute("disabled")).toBe(true);
  });

  it("a click opens a folder and ticks a file", () => {
    const h = render();
    onDownload(h);
    h.rows()[0]?.click();
    expect(h.dispatched).toContainEqual({
      type: "download/intent",
      intent: { kind: "browse", path: `${DIR}/sub` },
    });
    h.rows()[1]?.click();
    expect(h.app.store.getState().download?.selected).toEqual(["notes.txt"]);
    const ticked = h.rows()[1];
    expect(ticked?.getAttribute("aria-selected")).toBe("true");
    expect((ticked?.querySelector("input") as HTMLInputElement).checked).toBe(true);
    expect(h.q('.fd-download [data-total]').textContent).toBe("1 file, 23 bytes");
    expect(h.q('.fd-download [data-action="download"]').hasAttribute("disabled")).toBe(false);
    h.q('.fd-download [data-action="download"]').click();
    expect(h.dispatched).toContainEqual({ type: "download/submit" });
  });

  it("selects all, then none", () => {
    const h = render();
    onDownload(h);
    h.q('.fd-download [data-action="select-all"]').click();
    expect(h.app.store.getState().download?.selected).toEqual(["notes.txt", "data.bin"]);
    expect(h.q('.fd-download [data-action="select-all"]').textContent).toBe("Select none");
    expect(h.q('.fd-download [data-total]').textContent).toBe("2 files, 2.0 KB");
    h.q('.fd-download [data-action="select-all"]').click();
    expect(h.app.store.getState().download?.selected).toEqual([]);
  });

  it("walks with ↑↓, opens a folder with Enter, and ticks a file with Space", () => {
    const h = render();
    onDownload(h);
    const list = h.q('[data-list="download"]');
    expect(h.rows()[0]?.getAttribute("data-cursor")).toBe("true");
    expect(list.getAttribute("aria-activedescendant")).toBe("fd-download-row-0");
    const enter = keyOn(list, "Enter");
    expect(enter.defaultPrevented).toBe(true);
    expect(h.dispatched).toContainEqual({
      type: "download/intent",
      intent: { kind: "browse", path: `${DIR}/sub` },
    });
    keyOn(list, "ArrowDown");
    expect(h.rows()[1]?.getAttribute("data-cursor")).toBe("true");
    expect(list.getAttribute("aria-activedescendant")).toBe("fd-download-row-1");
    const space = keyOn(list, " ");
    expect(space.defaultPrevented).toBe(true);
    expect(h.app.store.getState().download?.selected).toEqual(["notes.txt"]);
    keyOn(list, "ArrowDown");
    keyOn(list, "Enter");
    expect(h.app.store.getState().download?.selected).toEqual(["notes.txt", "data.bin"]);
    keyOn(list, "ArrowUp");
    expect(h.app.store.getState().download?.cursor).toBe(1);
    /** Keys on the list never reach the app's shortcuts. */
    keyOn(list, "a");
    expect(h.app.store.getState().feedOpen).toBe(false);
  });

  it("locks the list while asking, with Cancel", () => {
    const h = render();
    onDownload(h);
    h.rows()[1]?.click();
    h.app.store.dispatch({ type: "download/requested", seq: 5 });
    const ask = h.q('.fd-download [data-action="download"]');
    expect(ask.textContent).toBe("Asking the worker…");
    expect(ask.hasAttribute("disabled")).toBe(true);
    expect((h.rows()[2]?.querySelector("input") as HTMLInputElement).disabled).toBe(true);
    expect(h.q('.fd-download [data-action="home"]').hasAttribute("disabled")).toBe(true);
    h.rows()[2]?.click();
    expect(h.app.store.getState().download?.selected).toEqual(["notes.txt"]);
    h.q('.fd-download [data-action="cancel"]').click();
    expect(h.dispatched).toContainEqual({ type: "download/cancel" });
    expect(h.q(".fd-download [data-summary]").textContent).toContain("Download cancelled.");
  });

  it("says a refusal on its note line", () => {
    const h = render();
    onDownload(h);
    h.app.store.dispatch({ type: "download/note", note: NO_SELECTION });
    const note = h.q(".fd-download .fd-upload__note");
    expect(visible(note)).toBe(true);
    expect(note.textContent).toBe(NO_SELECTION);
  });

  it("says where the files went, with Download more and Close", () => {
    const h = render();
    onDownload(h);
    h.q('.fd-download [data-action="select-all"]').click();
    h.app.store.dispatch({ type: "download/requested", seq: 5 });
    h.app.store.dispatch({
      type: "download/plan",
      plan: {
        seq: 5,
        dir: DIR,
        files: [
          { name: "notes.txt", size: 23, ticket: "t-1" },
          { name: "data.bin", size: 2048, ticket: "t-2" },
        ],
        refusal: null,
      },
    });
    h.app.store.dispatch({ type: "download/handedOff" });
    expect(visible(h.q('.fd-download [data-section="browse"]'))).toBe(false);
    expect(h.q(".fd-download [data-summary]").textContent).toBe(
      "Sent 2 files to your browser's downloads.",
    );
    expect(h.q(".fd-download").textContent).toContain(MULTIPLE_HINT);
    expect(
      [...h.app.el.querySelectorAll('[data-list="handed-off"] li')].map((li) =>
        li.getAttribute("data-name"),
      ),
    ).toEqual(["notes.txt", "data.bin"]);
    h.q('.fd-download [data-action="download-more"]').click();
    expect(h.dispatched).toContainEqual({ type: "download/startOver" });
    expect(visible(h.q('.fd-download [data-section="browse"]'))).toBe(true);
    h.app.store.dispatch({ type: "download/requested", seq: 6 });
    h.app.store.dispatch({ type: "download/cancel" });
    h.q('.fd-download [data-action="close-done"]').click();
    expect(h.app.store.getState().upload).toBeNull();
    expect(visible(h.q(".fd-upload"))).toBe(false);
  });

  it("Esc closes the panel from the Download tab", () => {
    const h = render();
    onDownload(h);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(h.app.store.getState().upload).toBeNull();
    expect(h.app.store.getState().download).toBeNull();
  });

  it("is never shown on a worker at v8", () => {
    const h = render(workerSnapshotWire({ protocol_version: 8 }));
    h.app.store.dispatch({ type: "transfer/open", tab: "download" });
    expect(h.q(".fd-upload").getAttribute("data-tab")).toBe("upload");
    expect(visible(h.q("#fd-transfer-download"))).toBe(false);
    expect(visible(h.q(".fd-transfer__tabs"))).toBe(false);
  });
});
