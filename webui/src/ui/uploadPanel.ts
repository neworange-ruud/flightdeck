import { tildePath } from "../state/picker";
import {
  doneLine,
  humanSize,
  isUploading,
  progressBytes,
  totalBytes,
  uploadAvailable,
  workerName,
} from "../state/upload";
import type { Picked, UploadItem, UploadState } from "../state/upload";
import type { AppState } from "../state/types";
import { append, clear, el, hint } from "./dom";
import type { Child, Region } from "./dom";
import type { Store } from "./store";

/**
 * Upload files to a worker (`specs/WORKER_PRD.md` W34, `specs/WEB_INTERFACE.md`
 * R30, `remote-control-usem`).
 *
 * ```text
 * Upload files to the worker                                 Esc close
 * note line (a refusal, a skipped folder — the worker's words verbatim)
 * FILES     name ............................. 1.2 MB   Remove
 *           Add files…                         2 files, 3.4 MB
 * FOLDER    [~/src/my-repo                     ] Choose…
 *                                                            Upload
 * ```
 *
 * then, by phase: *asking* lists the names already there with Replace / Keep
 * both / Cancel; *uploading* is a `<progress>` per file and one for the whole,
 * with Cancel; *done* is a summary with every failure's reason, Close and
 * Upload more.
 *
 * Everything it shows is `state.upload`, and every click is an action —
 * `main.ts` sends what they mean. Three things here are not state, because
 * they are the browser's own furniture:
 *
 * - **The hidden `<input type="file" multiple>`** (`input`), which both entry
 *   points open (`chooseFiles`) and whose `change` becomes `upload/open`.
 *   Opening a file chooser must happen inside the click or key press that
 *   asked for it, so it is a call, not an action.
 * - **The drop overlay** (`installDropTarget`), shown while files are
 *   dragged over a worker's window.
 * - **The folder field**, built once like the picker's, so typing is never
 *   overwritten by its own echo.
 */
export interface UploadPanelRegion extends Region {
  /** The hidden file input; `app.ts` puts it in the frame. */
  readonly input: HTMLInputElement;
  /** The drop overlay; `app.ts` puts it in the frame. */
  readonly dropzone: HTMLElement;
  /** Open the browser's file chooser. Must run inside a user gesture. */
  chooseFiles(): void;
}

export function createUploadPanel(store: Store): UploadPanelRegion {
  const input = el("input", {
    class: "fd-upload__input",
    attrs: {
      type: "file",
      multiple: "",
      tabindex: "-1",
      "aria-hidden": "true",
      "data-upload-input": "",
    },
  });
  input.hidden = true;
  input.addEventListener("change", () => {
    const picked = pickedFromFiles(input.files);
    /** Cleared, so choosing the same file again is still a `change`. */
    input.value = "";
    if (picked.length > 0) {
      store.dispatch({ type: "upload/open", picked });
    }
  });

  const dirInput = el("input", {
    class: "fd-picker__input fd-upload__dir",
    attrs: {
      type: "text",
      name: "upload-dir",
      "aria-label": "Folder on the worker",
      autocomplete: "off",
      spellcheck: "false",
      autocapitalize: "off",
    },
  });
  let renderedDir = "";
  dirInput.addEventListener("input", () => {
    renderedDir = dirInput.value;
    store.dispatch({ type: "upload/dir", dir: dirInput.value });
  });
  dirInput.addEventListener("keydown", (event: KeyboardEvent) => {
    if (event.key === "Enter") {
      event.preventDefault();
      store.dispatch({ type: "upload/submit" });
    }
  });

  const closeEl = el(
    "button",
    {
      class: "fd-picker__close",
      attrs: { type: "button", "data-action": "close", "aria-label": "Close" },
    },
    [el("span", { class: "fd-key", text: "Esc" }), " close"],
  );
  closeEl.addEventListener("click", () => store.dispatch({ type: "upload/close" }));

  const noteEl = el("p", {
    class: "fd-upload__note",
    attrs: { role: "alert" },
  });
  const body = el("div", { class: "fd-upload__body" });
  /** The folder section is built once, so the field keeps its caret and
   * focus while the rest of the panel re-renders around it. */
  const chooseEl = button("Choose…", "choose-folder", () => {
    const upload = store.getState().upload;
    if (upload !== null) {
      store.dispatch({
        type: "picker/request",
        intent: { kind: "choose_folder", path: upload.dir },
      });
    }
  });
  const dirShown = el("span", { class: "fd-picker__name fd-upload__dir-shown" });
  const folder = section("FOLDER", [
    el("div", { class: "fd-picker__row" }, [dirInput, dirShown, chooseEl]),
    el("p", {
      class: "fd-picker__muted",
      text: "Inside the worker's home folder. Drop files on the window to add them.",
    }),
  ]);
  folder.setAttribute("data-section", "folder");
  const foot = el("div", { class: "fd-upload__foot" });

  const panel = el("div", { class: "fd-picker__panel fd-upload__panel" }, [
    el("div", { class: "fd-picker__head" }, [
      el("span", {
        class: "fd-picker__title",
        text: "Upload files to the worker",
        attrs: { id: "fd-upload-title" },
      }),
      closeEl,
    ]),
    noteEl,
    body,
    folder,
    foot,
  ]);

  const layer = el(
    "div",
    {
      class: "fd-upload",
      attrs: {
        role: "dialog",
        "aria-modal": "true",
        "aria-labelledby": "fd-upload-title",
      },
    },
    [panel],
  );

  const dropzone = el(
    "div",
    { class: "fd-dropzone", attrs: { "aria-hidden": "true" } },
    [el("p", { class: "fd-dropzone__label" })],
  );
  dropzone.hidden = true;

  let wasOpen = false;

  function render(state: AppState): void {
    const upload = state.upload;
    const label = dropzone.firstElementChild;
    if (label !== null) {
      label.textContent = `Drop files to upload to ${workerName(state)}`;
    }
    /** Under Choose…'s folder picker, the panel waits out of sight. */
    layer.hidden = upload === null || state.picker !== null;
    if (upload === null) {
      wasOpen = false;
      return;
    }
    layer.setAttribute("data-phase", upload.phase.kind);
    noteEl.textContent = upload.note ?? "";
    noteEl.hidden = upload.note === null;
    closeEl.hidden = isUploading(upload);

    clear(body);
    clear(foot);
    const home = state.worker?.home ?? "";
    const view = phaseView(upload, home, store, chooseFiles);
    append(body, view.body);
    append(foot, view.foot);

    const choosing = upload.phase.kind === "choosing";
    folder.hidden = !(choosing || upload.phase.kind === "requested");
    dirInput.hidden = !choosing;
    dirShown.hidden = choosing;
    dirShown.textContent = upload.dir;
    chooseEl.disabled = !choosing;
    if (renderedDir !== upload.dir) {
      renderedDir = upload.dir;
      dirInput.value = upload.dir;
    }

    if (!wasOpen && !layer.hidden) {
      wasOpen = true;
      /** Focus lands on the panel's main button when it opens, so `Enter`
       * uploads and `Tab` walks the list — but only then. */
      queueMicrotask(() => {
        if (layer.isConnected && !layer.hidden) {
          (
            layer.querySelector<HTMLElement>('[data-primary="true"]:not([disabled])') ??
            layer.querySelector<HTMLElement>("button:not([disabled])")
          )?.focus();
        }
      });
    }
  }

  function chooseFiles(): void {
    const state = store.getState();
    if (!uploadAvailable(state)) {
      return;
    }
    if (state.upload !== null && state.upload.phase.kind !== "choosing") {
      return;
    }
    input.click();
  }

  return { el: layer, input, dropzone, update: render, chooseFiles };
}

/** What a `<input type=file>` handed over, as picks. A chooser never offers
 * folders, so every entry is a file. */
export function pickedFromFiles(files: FileList | readonly File[] | null): Picked[] {
  if (files === null) {
    return [];
  }
  return Array.from(files, (file) => ({
    kind: "file" as const,
    file: { name: file.name, size: file.size, source: file },
  }));
}

/**
 * What a drop handed over, as picks. A folder is told apart by its entry
 * (`webkitGetAsEntry`), since its `File` looks like an empty file; an engine
 * without entries falls back to the plain file list.
 */
export function pickedFromDataTransfer(data: DataTransfer): Picked[] {
  const items = data.items;
  if (items === undefined || items.length === 0) {
    return pickedFromFiles(data.files);
  }
  const picked: Picked[] = [];
  for (const item of Array.from(items)) {
    if (item.kind !== "file") {
      continue;
    }
    const entry =
      typeof item.webkitGetAsEntry === "function" ? item.webkitGetAsEntry() : null;
    const file = item.getAsFile();
    if (entry !== null && entry.isDirectory) {
      picked.push({ kind: "folder", name: entry.name });
      continue;
    }
    if (file === null) {
      picked.push({
        kind: "unreadable",
        name: entry?.name ?? "a dropped item",
        why: "the browser gave no file for it",
      });
      continue;
    }
    picked.push({ kind: "file", file: { name: file.name, size: file.size, source: file } });
  }
  return picked;
}

/** Whether a drag carries files — not text, not a link. */
function carriesFiles(event: DragEvent): boolean {
  const types = event.dataTransfer?.types;
  return types !== undefined && Array.from(types).includes("Files");
}

/**
 * Dropping files on a worker's window (R30): an overlay while they are over
 * it, and the panel — opened, or added to while it is still choosing — when
 * they land. Only a drag that carries files is claimed, so a text drag (and
 * whatever xterm does with one) is untouched, and nothing happens on any host
 * but a worker this window can upload to.
 */
export function installDropTarget(
  frame: HTMLElement,
  store: Store,
  dropzone: HTMLElement,
): void {
  /** `dragenter`/`dragleave` fire for every child crossed; a depth count is
   * how the overlay knows the drag really left the window. */
  let depth = 0;
  const show = (on: boolean): void => {
    dropzone.hidden = !on;
    frame.setAttribute("data-dropping", String(on));
  };
  const accepting = (event: DragEvent): boolean => {
    const state = store.getState();
    if (!carriesFiles(event) || !uploadAvailable(state)) {
      return false;
    }
    return state.upload === null || state.upload.phase.kind === "choosing";
  };

  frame.addEventListener("dragenter", (event: DragEvent) => {
    if (!accepting(event)) {
      return;
    }
    event.preventDefault();
    depth += 1;
    show(true);
  });
  frame.addEventListener("dragover", (event: DragEvent) => {
    if (!accepting(event)) {
      return;
    }
    event.preventDefault();
    if (event.dataTransfer !== null) {
      event.dataTransfer.dropEffect = "copy";
    }
    show(true);
  });
  frame.addEventListener("dragleave", (event: DragEvent) => {
    if (!carriesFiles(event)) {
      return;
    }
    depth = Math.max(0, depth - 1);
    if (depth === 0) {
      show(false);
    }
  });
  frame.addEventListener("drop", (event: DragEvent) => {
    const wanted = accepting(event);
    depth = 0;
    show(false);
    if (!wanted || event.dataTransfer === null) {
      return;
    }
    event.preventDefault();
    const picked = pickedFromDataTransfer(event.dataTransfer);
    if (picked.length > 0) {
      store.dispatch({ type: "upload/open", picked });
    }
  });
}

// ---------------------------------------------------------------------------
// One view per phase
// ---------------------------------------------------------------------------

interface PhaseView {
  readonly body: Child[];
  readonly foot: Child[];
}

function phaseView(
  upload: UploadState,
  home: string,
  store: Store,
  chooseFiles: () => void,
): PhaseView {
  const phase = upload.phase;
  switch (phase.kind) {
    case "choosing":
    case "requested": {
      const asking = phase.kind === "requested";
      const add = button("Add files…", "add-files", chooseFiles, asking);
      const submit = button(
        asking ? "Asking the worker…" : "Upload",
        "upload",
        () => store.dispatch({ type: "upload/submit" }),
        asking || upload.files.length === 0,
        true,
      );
      return {
        body: [
          section("FILES", [
            fileList(upload, store, asking),
            el("div", { class: "fd-picker__row" }, [
              add,
              el("span", { class: "fd-spacer" }),
              el("span", {
                class: "fd-picker__detail",
                attrs: { "data-total": "" },
                text: filesLine(upload.files.length, totalBytes(upload)),
              }),
            ]),
          ]),
        ],
        foot: [
          hint("Esc", "close"),
          el("span", { class: "fd-spacer" }),
          asking
            ? button("Cancel", "cancel", () => store.dispatch({ type: "upload/cancel" }))
            : null,
          submit,
        ],
      };
    }
    case "asking": {
      const count = phase.conflicts.length;
      return {
        body: [
          section("ALREADY THERE", [
            el("p", {
              class: "fd-upload__line",
              text:
                count === 1
                  ? `A file with this name is already in ${tildePath(phase.dir, home)}:`
                  : `${count} files with these names are already in ${tildePath(phase.dir, home)}:`,
            }),
            el(
              "ul",
              { class: "fd-upload__list", attrs: { "data-list": "conflicts" } },
              phase.conflicts.map((name) =>
                el("li", { class: "fd-picker__item", attrs: { "data-name": name } }, [
                  el("span", { class: "fd-picker__name", text: name }),
                ]),
              ),
            ),
            el("p", {
              class: "fd-picker__muted",
              text: "Replace overwrites them. Keep both saves the new files as name (1).ext.",
            }),
          ]),
        ],
        foot: [
          el("span", { class: "fd-spacer" }),
          button("Cancel", "cancel", () =>
            store.dispatch({ type: "upload/resolve", resolution: "cancel" }),
          ),
          button("Replace", "replace", () =>
            store.dispatch({ type: "upload/resolve", resolution: "replace" }),
          ),
          button(
            "Keep both",
            "keep-both",
            () => store.dispatch({ type: "upload/resolve", resolution: "keep_both" }),
            false,
            true,
          ),
        ],
      };
    }
    case "uploading": {
      const bytes = progressBytes(upload) ?? { sent: 0, total: 0 };
      return {
        body: [
          section(`UPLOADING TO ${tildePath(phase.dir, home)}`, [
            el(
              "ul",
              { class: "fd-upload__list", attrs: { "data-list": "progress" } },
              phase.items.map((item) => progressRow(item)),
            ),
            el("div", { class: "fd-upload__total" }, [
              progressBar(bytes.sent, bytes.total, "Total"),
              el("span", {
                class: "fd-picker__detail",
                attrs: { "data-total": "" },
                text: `${humanSize(bytes.sent)} of ${humanSize(bytes.total)}`,
              }),
            ]),
          ]),
        ],
        foot: [
          el("span", { class: "fd-spacer" }),
          button("Cancel", "cancel", () => store.dispatch({ type: "upload/cancel" }), false, true),
        ],
      };
    }
    case "done": {
      return {
        body: [
          section("DONE", [
            el("p", {
              class: "fd-upload__line",
              attrs: { "data-summary": "" },
              text: doneLine(
                phase.written.length,
                phase.failed.length,
                tildePath(phase.dir, home),
              ),
            }),
            el(
              "ul",
              { class: "fd-upload__list", attrs: { "data-list": "done" } },
              [
                ...phase.written.map((name) =>
                  el(
                    "li",
                    {
                      class: "fd-picker__item",
                      attrs: { "data-name": name, "data-state": "written" },
                    },
                    [
                      el("span", { class: "fd-glyph fd-tone-ok", text: "✓", attrs: { "aria-hidden": "true" } }),
                      el("span", { class: "fd-picker__name", text: name }),
                    ],
                  ),
                ),
                ...phase.failed.map(({ name, why }) =>
                  el(
                    "li",
                    {
                      class: "fd-picker__item",
                      attrs: { "data-name": name, "data-state": "failed" },
                    },
                    [
                      el("span", { class: "fd-glyph fd-tone-alert", text: "✗", attrs: { "aria-hidden": "true" } }),
                      el("span", { class: "fd-picker__name", text: name }),
                      el("span", { class: "fd-upload__why", text: why }),
                    ],
                  ),
                ),
              ],
            ),
          ]),
        ],
        foot: doneFoot(store),
      };
    }
    case "cancelled":
      return {
        body: [
          section("CANCELLED", [
            el("p", {
              class: "fd-upload__line",
              attrs: { "data-summary": "" },
              text: "Upload cancelled. Any file already sent stays on the worker.",
            }),
          ]),
        ],
        foot: doneFoot(store),
      };
  }
}

function doneFoot(store: Store): Child[] {
  return [
    el("span", { class: "fd-spacer" }),
    button("Upload more", "upload-more", () => store.dispatch({ type: "upload/startOver" })),
    button("Close", "close-done", () => store.dispatch({ type: "upload/close" }), false, true),
  ];
}

function fileList(upload: UploadState, store: Store, locked: boolean): HTMLElement {
  if (upload.files.length === 0) {
    return el("p", {
      class: "fd-picker__muted",
      text: "No files yet. Add some, or drop them on the window.",
    });
  }
  return el(
    "ul",
    { class: "fd-upload__list", attrs: { "data-list": "files" } },
    upload.files.map((file, index) => {
      const remove = button(
        "Remove",
        "remove",
        () => store.dispatch({ type: "upload/remove", index }),
        locked,
      );
      remove.setAttribute("aria-label", `Remove ${file.name}`);
      return el("li", { class: "fd-picker__item", attrs: { "data-name": file.name } }, [
        el("span", { class: "fd-picker__name", text: file.name }),
        el("span", { class: "fd-spacer" }),
        el("span", { class: "fd-picker__detail", text: humanSize(file.size) }),
        remove,
      ]);
    }),
  );
}

function progressRow(item: UploadItem): HTMLElement {
  const renamed = item.finalName !== item.name;
  return el(
    "li",
    {
      class: "fd-picker__item fd-upload__item",
      attrs: { "data-name": item.name, "data-state": item.state.kind },
    },
    [
      el("span", {
        class: "fd-picker__name",
        text: renamed ? `${item.name} → ${item.finalName}` : item.name,
      }),
      progressBar(item.sent, item.total, item.finalName),
      el("span", { class: "fd-picker__detail fd-upload__state", text: itemLine(item) }),
    ],
  );
}

function itemLine(item: UploadItem): string {
  switch (item.state.kind) {
    case "waiting":
      return "waiting";
    case "sending":
      return item.total === 0
        ? "sending"
        : `${Math.floor((item.sent / item.total) * 100)}%`;
    case "written":
      return "done";
    case "failed":
      return item.state.why;
  }
}

function progressBar(value: number, max: number, label: string): HTMLProgressElement {
  const bar = el("progress", {
    class: "fd-upload__progress",
    attrs: { "aria-label": `${label} progress` },
  });
  /** A zero-byte file is complete the moment it is sent. */
  bar.max = Math.max(1, max);
  bar.value = max === 0 ? 0 : Math.min(value, max);
  return bar;
}

function filesLine(count: number, bytes: number): string {
  if (count === 0) {
    return "";
  }
  return `${count === 1 ? "1 file" : `${count} files`}, ${humanSize(bytes)}`;
}

function section(title: string, children: Child[]): HTMLElement {
  return el("section", { class: "fd-picker__section" }, [
    el("div", { class: "fd-picker__section-head" }, [
      el("span", { class: "fd-picker__section-title", text: title }),
    ]),
    ...children,
  ]);
}

function button(
  label: string,
  action: string,
  onClick: () => void,
  disabled = false,
  primary = false,
): HTMLButtonElement {
  const attrs: Record<string, string> = { type: "button", "data-action": action };
  if (disabled) {
    attrs.disabled = "";
  }
  if (primary) {
    attrs["data-primary"] = "true";
  }
  const node = el("button", {
    class: primary ? "fd-picker__button" : "fd-picker__button fd-upload__secondary",
    text: label,
    attrs,
  });
  node.addEventListener("click", onClick);
  return node;
}
