/**
 * @vitest-environment jsdom
 *
 * The upload panel, rendered (W34, R30): where its entry points appear (a
 * worker at v8 — never a TUI host, never a v7 peer), the panel through its
 * phases, the keyboard, and a drop. Everything is driven through the store;
 * `main.ts`'s sending is out of scope, so what a click *means* is read off
 * the dispatched actions.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fixtureSnapshot } from "../state/fixture";
import type { Picked } from "../state/upload";
import type { AppAction } from "../state/types";
import { snapshotFromWire } from "../wire/adapt";
import type { WireCommandView, WireSnapshot } from "../wire/frames";
import { WORKER_HOME, workerSnapshotWire } from "../wire/workerFixture";
import { createApp } from "./app";
import { pickedFromDataTransfer } from "./uploadPanel";

const UPLOAD_ROW: WireCommandView = {
  id: "upload_files",
  label: "Upload Files…",
  group: "Projects",
  run: { name: "upload_files" },
  refusal: "Use the window's upload panel.",
};

function render(snapshot: WireSnapshot | "tui" = workerSnapshotWire({ commands: [UPLOAD_ROW] })) {
  const dispatched: AppAction[] = [];
  const ran: string[] = [];
  const app = createApp({
    mount: (container, _geometry, terminalId) => {
      container.append(`[${terminalId}]`);
      return { dispose: () => undefined, resize: () => undefined };
    },
    onDispatch: (action) => dispatched.push(action),
    onRunCommand: (command) => ran.push(command.run.name),
  });
  document.body.append(app.el);
  app.store.dispatch({
    type: "snapshot/received",
    snapshot: snapshot === "tui" ? fixtureSnapshot() : snapshotFromWire(snapshot),
  });
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
  return { app, dispatched, ran, q, maybe };
}

function file(name: string, text = "abc"): File {
  return new File([text], name);
}

function pick(...files: File[]): Picked[] {
  return files.map((f) => ({ kind: "file", file: { name: f.name, size: f.size, source: f } }));
}

function visible(element: HTMLElement | null): boolean {
  return element !== null && !element.hidden;
}

function key(name: string): void {
  document.dispatchEvent(new KeyboardEvent("keydown", { key: name, bubbles: true }));
}

let click: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  document.body.replaceChildren();
  click = vi.spyOn(HTMLInputElement.prototype, "click").mockImplementation(() => undefined);
});

afterEach(() => {
  click.mockRestore();
});

describe("where Upload files appears", () => {
  it("beside Open Project on a worker at v8, opening the file chooser", () => {
    const h = render();
    const button = h.q('.fd-projects [data-action="upload-files"]');
    expect(button.textContent).toBe("Upload files");
    expect(button.nextElementSibling?.getAttribute("data-action")).toBe("open-project");
    button.click();
    expect(click).toHaveBeenCalledTimes(1);
    expect(h.q("[data-upload-input]").getAttribute("type")).toBe("file");
    expect(h.q("[data-upload-input]").hasAttribute("multiple")).toBe(true);
  });

  it("nowhere on a TUI host", () => {
    const h = render("tui");
    expect(h.maybe('[data-action="upload-files"]')).toBeNull();
    h.app.store.dispatch({ type: "upload/open", picked: pick(file("a.txt")) });
    expect(h.app.store.getState().upload).toBeNull();
    expect(visible(h.maybe(".fd-upload"))).toBe(false);
  });

  it("nowhere on a worker that answered at v7", () => {
    const h = render(workerSnapshotWire({ protocol_version: 7, commands: [UPLOAD_ROW] }));
    expect(h.maybe('[data-action="upload-files"]')).toBeNull();
    h.app.store.dispatch({ type: "palette/open" });
    expect(h.q(".fd-palette").textContent).not.toContain("Upload Files…");
  });

  it("in the palette, where it opens the chooser rather than being sent", () => {
    const h = render();
    h.app.store.dispatch({ type: "palette/open" });
    const row = [...h.app.el.querySelectorAll<HTMLElement>(".fd-palette__row")].find((r) =>
      (r.textContent ?? "").includes("Upload Files…"),
    );
    expect(row).toBeDefined();
    expect(row?.textContent).not.toContain("Use the window's upload panel.");
    row?.click();
    expect(click).toHaveBeenCalledTimes(1);
    expect(h.ran).toEqual([]);
    expect(h.app.store.getState().palette).toBeNull();
  });
});

describe("the panel", () => {
  it("opens with the chosen files, their sizes, and the default folder", () => {
    const h = render();
    const input = h.q("[data-upload-input]") as HTMLInputElement;
    Object.defineProperty(input, "files", {
      configurable: true,
      value: [file("a.txt", "hello"), file("b.bin", "x".repeat(2048))],
    });
    input.dispatchEvent(new Event("change"));
    expect(visible(h.q(".fd-upload"))).toBe(true);
    expect(h.app.el.getAttribute("data-upload")).toBe("choosing");
    const rows = [...h.app.el.querySelectorAll<HTMLElement>('[data-list="files"] li')];
    expect(rows.map((r) => r.getAttribute("data-name"))).toEqual(["a.txt", "b.bin"]);
    expect(rows[0]?.textContent).toContain("5 bytes");
    expect(rows[1]?.textContent).toContain("2.0 KB");
    expect((h.q(".fd-upload__dir") as HTMLInputElement).value).toBe(WORKER_HOME);
    expect(h.q('.fd-upload [data-total]').textContent).toBe("2 files, 2.0 KB");
    expect(h.q(".fd-upload").getAttribute("role")).toBe("dialog");
  });

  it("removes a file, types a folder, and asks to upload", () => {
    const h = render();
    h.app.store.dispatch({ type: "upload/open", picked: pick(file("a.txt"), file("b.txt")) });
    h.q('.fd-upload [data-name="a.txt"] [data-action="remove"]').click();
    expect(h.app.store.getState().upload?.files.map((f) => f.name)).toEqual(["b.txt"]);
    const dir = h.q(".fd-upload__dir") as HTMLInputElement;
    dir.value = "/home/me/data";
    dir.dispatchEvent(new Event("input"));
    expect(h.app.store.getState().upload?.dir).toBe("/home/me/data");
    /** The field survives the re-render its own echo causes. */
    expect(h.q(".fd-upload__dir")).toBe(dir);
    h.q('.fd-upload [data-action="upload"]').click();
    expect(h.dispatched).toContainEqual({ type: "upload/submit" });
  });

  it("Choose… asks for a browse-only picker, which hands back its folder", () => {
    const h = render();
    h.app.store.dispatch({ type: "upload/open", picked: pick(file("a.txt")) });
    h.q('.fd-upload [data-action="choose-folder"]').click();
    expect(h.dispatched).toContainEqual({
      type: "picker/request",
      intent: { kind: "choose_folder", path: WORKER_HOME },
    });
    expect(visible(h.q(".fd-upload"))).toBe(false);
    const picker = h.q(".fd-picker");
    expect(picker.getAttribute("data-purpose")).toBe("choose_folder");
    expect(picker.textContent).toContain("Choose a folder on the worker");
    expect(visible(h.q('.fd-picker [data-section="clone"]'))).toBe(false);
    expect(visible(h.q('.fd-picker [data-section="scan"]'))).toBe(false);
    h.app.store.dispatch({
      type: "picker/listing",
      listing: {
        seq: 0,
        path: `${WORKER_HOME}/data`,
        parent: WORKER_HOME,
        home: WORKER_HOME,
        entries: [],
        truncated: false,
      },
    });
    /** No browse was sent in this test, so the listing is not its answer. */
    expect(h.q('.fd-picker [data-action="choose-here"]').hasAttribute("disabled")).toBe(true);
    h.app.store.dispatch({ type: "picker/sent", sent: { kind: "browse" }, seq: 3 });
    h.app.store.dispatch({
      type: "picker/listing",
      listing: {
        seq: 3,
        path: `${WORKER_HOME}/data`,
        parent: WORKER_HOME,
        home: WORKER_HOME,
        entries: [],
        truncated: false,
      },
    });
    h.q('.fd-picker [data-action="choose-here"]').click();
    expect(h.app.store.getState().picker).toBeNull();
    expect(h.app.store.getState().upload?.dir).toBe(`${WORKER_HOME}/data`);
    expect(visible(h.q(".fd-upload"))).toBe(true);
  });

  it("asks about taken names with Replace, Keep both and Cancel", () => {
    const h = render();
    h.app.store.dispatch({ type: "upload/open", picked: pick(file("a.txt")) });
    h.app.store.dispatch({ type: "upload/requested", seq: 5, onConflict: "ask" });
    expect(h.q('.fd-upload [data-action="upload"]').hasAttribute("disabled")).toBe(true);
    h.app.store.dispatch({
      type: "upload/plan",
      plan: {
        seq: 5,
        dir: `${WORKER_HOME}/x`,
        files: [{ name: "a.txt", finalName: "a.txt", ticket: null, exists: true }],
        conflicts: true,
        refusal: null,
      },
    });
    expect(h.q('[data-list="conflicts"]').textContent).toContain("a.txt");
    expect(h.q(".fd-upload").textContent).toContain("~/x");
    h.q('.fd-upload [data-action="keep-both"]').click();
    expect(h.dispatched).toContainEqual({ type: "upload/resolve", resolution: "keep_both" });
    h.q('.fd-upload [data-action="cancel"]').click();
    expect(h.app.store.getState().upload?.phase).toEqual({ kind: "choosing" });
  });

  it("shows each file's progress and the total, and Esc does not close it", () => {
    const h = render();
    h.app.store.dispatch({ type: "upload/open", picked: pick(file("a.txt", "abcd")) });
    h.app.store.dispatch({ type: "upload/requested", seq: 5, onConflict: "ask" });
    h.app.store.dispatch({
      type: "upload/plan",
      plan: {
        seq: 5,
        dir: WORKER_HOME,
        files: [{ name: "a.txt", finalName: "a.txt", ticket: "t", exists: false }],
        conflicts: false,
        refusal: null,
      },
    });
    h.app.store.dispatch({ type: "upload/started", index: 0 });
    h.app.store.dispatch({ type: "upload/progress", index: 0, sent: 2 });
    const bars = [...h.app.el.querySelectorAll<HTMLProgressElement>(".fd-upload progress")];
    expect(bars.map((b) => [b.value, b.max])).toEqual([
      [2, 4],
      [2, 4],
    ]);
    expect(h.q('.fd-upload [data-state="sending"]').textContent).toContain("50%");
    expect(visible(h.q('.fd-upload [data-action="close"]'))).toBe(false);
    key("Escape");
    expect(h.app.store.getState().upload).not.toBeNull();
    h.q('.fd-upload [data-action="cancel"]').click();
    expect(h.dispatched).toContainEqual({ type: "upload/cancel" });
    expect(h.app.el.getAttribute("data-upload")).toBe("cancelled");
  });

  it("sums up what was written and why the rest was not", () => {
    const h = render();
    h.app.store.dispatch({ type: "upload/open", picked: pick(file("a.txt"), file("b.txt")) });
    h.app.store.dispatch({ type: "upload/requested", seq: 5, onConflict: "ask" });
    h.app.store.dispatch({
      type: "upload/plan",
      plan: {
        seq: 5,
        dir: WORKER_HOME,
        files: [
          { name: "a.txt", finalName: "a.txt", ticket: "t1", exists: false },
          { name: "b.txt", finalName: "b.txt", ticket: "t2", exists: false },
        ],
        conflicts: false,
        refusal: null,
      },
    });
    h.app.store.dispatch({ type: "upload/started", index: 0 });
    h.app.store.dispatch({
      type: "upload/finished",
      index: 0,
      result: { ok: true, path: `${WORKER_HOME}/a.txt` },
    });
    h.app.store.dispatch({ type: "upload/started", index: 1 });
    h.app.store.dispatch({
      type: "upload/finished",
      index: 1,
      result: { ok: false, why: "No space left on the worker." },
    });
    expect(h.q(".fd-upload [data-summary]").textContent).toBe("Uploaded 1 file to ~; 1 failed.");
    expect(h.q('.fd-upload [data-state="failed"]').textContent).toContain(
      "No space left on the worker.",
    );
    h.q('.fd-upload [data-action="upload-more"]').click();
    expect(h.app.store.getState().upload?.files).toEqual([]);
    h.app.store.dispatch({ type: "upload/cancel" });
    h.q('.fd-upload [data-action="close-done"]').click();
    expect(h.app.store.getState().upload).toBeNull();
  });

  it("says a refusal on its note line, and Esc closes it while choosing", () => {
    const h = render();
    h.app.store.dispatch({ type: "upload/open", picked: pick(file("a.txt")) });
    h.app.store.dispatch({ type: "upload/note", note: "That folder is outside your home." });
    expect(h.q(".fd-upload__note").textContent).toBe("That folder is outside your home.");
    expect(visible(h.q(".fd-upload__note"))).toBe(true);
    key("a");
    expect(h.app.store.getState().feedOpen).toBe(false);
    key("Escape");
    expect(h.app.store.getState().upload).toBeNull();
    expect(visible(h.q(".fd-upload"))).toBe(false);
  });
});

describe("dropping files", () => {
  function dragEvent(type: string, data: Partial<DataTransfer>): Event {
    const event = new Event(type, { bubbles: true, cancelable: true });
    Object.defineProperty(event, "dataTransfer", { value: data });
    return event;
  }

  function entryItem(f: File, directory = false): DataTransferItem {
    return {
      kind: "file",
      type: "",
      getAsFile: () => (directory ? null : f),
      getAsString: () => undefined,
      webkitGetAsEntry: () => ({ isDirectory: directory, isFile: !directory, name: f.name }),
    } as unknown as DataTransferItem;
  }

  function transfer(...items: DataTransferItem[]): Partial<DataTransfer> {
    return {
      types: ["Files"],
      items: items as unknown as DataTransferItemList,
      files: [] as unknown as FileList,
      dropEffect: "none",
    };
  }

  it("shows the overlay over a worker's window, and opens the panel on a drop", () => {
    const h = render();
    const data = transfer(entryItem(file("a.txt")), entryItem(file("photos"), true));
    const enter = dragEvent("dragenter", data);
    h.app.el.dispatchEvent(enter);
    expect(enter.defaultPrevented).toBe(true);
    expect(visible(h.q(".fd-dropzone"))).toBe(true);
    expect(h.q(".fd-dropzone").textContent).toBe(
      "Drop files to upload to box.tail1234.ts.net",
    );
    const drop = dragEvent("drop", data);
    h.app.el.dispatchEvent(drop);
    expect(drop.defaultPrevented).toBe(true);
    expect(visible(h.q(".fd-dropzone"))).toBe(false);
    const upload = h.app.store.getState().upload;
    expect(upload?.files.map((f) => f.name)).toEqual(["a.txt"]);
    expect(upload?.note).toBe("Folders cannot be uploaded; choose the files inside.");
  });

  it("leaves a text drag, and every other host, alone", () => {
    const h = render();
    const text = dragEvent("dragover", { types: ["text/plain"] });
    h.app.el.dispatchEvent(text);
    expect(text.defaultPrevented).toBe(false);
    expect(visible(h.q(".fd-dropzone"))).toBe(false);

    const tui = render("tui");
    const files = dragEvent("drop", transfer(entryItem(file("a.txt"))));
    tui.app.el.dispatchEvent(files);
    expect(files.defaultPrevented).toBe(false);
    expect(tui.app.store.getState().upload).toBeNull();
  });

  it("reads a plain file list when the browser gives no items", () => {
    const f = file("only.txt");
    expect(
      pickedFromDataTransfer({
        items: [] as unknown as DataTransferItemList,
        files: [f] as unknown as FileList,
      } as unknown as DataTransfer).map((p) => (p.kind === "file" ? p.file.name : p.kind)),
    ).toEqual(["only.txt"]);
  });
});
