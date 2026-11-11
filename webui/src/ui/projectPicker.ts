import { cloneLine, pickerBusy, tildePath } from "../state/picker";
import type { PickerIntent, PickerState } from "../state/picker";
import type { AppState } from "../state/types";
import { append, clear, el, hint } from "./dom";
import type { Child, Region } from "./dom";
import type { Store } from "./store";

const OPEN_TITLE = "Open a project on the worker";
/** The upload panel's Choose… (R30). */
const CHOOSE_TITLE = "Choose a folder on the worker";

/**
 * Open Project on a worker: the project picker (`specs/WORKER_PRD.md` W9,
 * W13, W16; `remote-control-vjtb`).
 *
 * ```text
 * Open a project on the worker                      Working…  Esc close
 * error line (the worker's words, verbatim)
 * PROJECTS IN ~/flightdeck     repo rows ......................  Open
 * BROWSE   [path                ] Go  Up  Open this folder  Show hidden
 *          folder rows (git ones with Open) ..........................
 * CLONE    [URL                 ] [folder name (optional)]      Clone
 *          progress line, or git's error
 * ```
 *
 * The native window's layout (`desktop/src/remote/projects.rs`), in this
 * app's vocabulary. Everything it shows is `state.picker`, and every click is
 * a `picker/request` that `main.ts` turns into the worker command — this
 * component sends nothing and decides nothing.
 *
 * **The three text fields are built once.** Every other region here rebuilds
 * its children on each render, which would throw away a caret and a half-typed
 * URL on every keystroke; these keep their element and are only written to
 * when the *state* moved under them (a listing moving the path bar), so typing
 * is never overwritten by its own echo.
 */
export function createProjectPicker(store: Store): Region {
  const request = (intent: PickerIntent): void =>
    store.dispatch({ type: "picker/request", intent });

  const pathInput = field("path", "~/src/my-repo", "Folder to list");
  const urlInput = field("url", "git@github.com:me/my-repo.git", "Repository URL");
  const nameInput = field("name", "folder name (optional)", "Folder name");
  const rendered = { path: "", url: "", name: "" };

  for (const [input, name] of [
    [pathInput, "path"],
    [urlInput, "url"],
    [nameInput, "name"],
  ] as const) {
    input.addEventListener("input", () => {
      rendered[name] = input.value;
      store.dispatch({ type: "picker/field", field: name, value: input.value });
    });
  }
  pathInput.addEventListener("keydown", (event: KeyboardEvent) => {
    if (event.key === "Enter") {
      event.preventDefault();
      request({ kind: "browse", path: pathInput.value });
    }
  });
  for (const input of [urlInput, nameInput]) {
    input.addEventListener("keydown", (event: KeyboardEvent) => {
      if (event.key === "Enter") {
        event.preventDefault();
        request({ kind: "clone" });
      }
    });
  }

  const busyEl = el("span", { class: "fd-picker__busy" });
  const closeEl = el(
    "button",
    { class: "fd-picker__close", attrs: { type: "button" } },
    [el("span", { class: "fd-key", text: "Esc" }), " close"],
  );
  closeEl.addEventListener("click", () => store.dispatch({ type: "picker/close" }));

  const errorEl = el("p", { class: "fd-picker__error", attrs: { role: "alert" } });
  const statusEl = el("p", { class: "fd-picker__status" });
  const scanHead = el("span", { class: "fd-picker__section-title" });
  const scanList = el("div", { class: "fd-picker__list", attrs: { "data-list": "scan" } });
  const browseTools = el("div", { class: "fd-picker__tools" });
  const browseList = el("div", { class: "fd-picker__list", attrs: { "data-list": "browse" } });
  const cloneTools = el("div", { class: "fd-picker__tools" });
  const cloneLineEl = el("p", { class: "fd-picker__clone-line" });

  const titleEl = el("span", { class: "fd-picker__title" });
  const scanSection = el(
    "section",
    { class: "fd-picker__section", attrs: { "data-section": "scan" } },
    [el("div", { class: "fd-picker__section-head" }, [scanHead]), scanList],
  );
  const cloneSection = el(
    "section",
    { class: "fd-picker__section", attrs: { "data-section": "clone" } },
    [
      el("div", { class: "fd-picker__section-head" }, [
        el("span", { class: "fd-picker__section-title", text: "CLONE" }),
      ]),
      el("div", { class: "fd-picker__row" }, [urlInput, nameInput, cloneTools]),
      cloneLineEl,
    ],
  );

  const panel = el("div", { class: "fd-picker__panel" }, [
    el("div", { class: "fd-picker__head" }, [titleEl, busyEl, closeEl]),
    errorEl,
    statusEl,
    scanSection,
    el("section", { class: "fd-picker__section", attrs: { "data-section": "browse" } }, [
      el("div", { class: "fd-picker__section-head" }, [
        el("span", { class: "fd-picker__section-title", text: "BROWSE" }),
      ]),
      el("div", { class: "fd-picker__row" }, [pathInput, browseTools]),
      browseList,
    ]),
    cloneSection,
    el("div", { class: "fd-picker__foot" }, [
      hint("Enter", "go / clone"),
      hint("Esc", "close"),
    ]),
  ]);

  const layer = el(
    "div",
    {
      class: "fd-picker",
      attrs: { role: "dialog", "aria-label": OPEN_TITLE },
    },
    [panel],
  );

  let wasOpen = false;

  function render(state: AppState): void {
    const picker = state.picker;
    layer.hidden = picker === null;
    if (picker === null) {
      wasOpen = false;
      return;
    }
    /** The upload panel's Choose… (R30) is this picker browsing only: no
     * scan, no clone, and the folder listed is the answer. */
    const choosing = picker.purpose === "choose_folder";
    const title = choosing ? CHOOSE_TITLE : OPEN_TITLE;
    titleEl.textContent = title;
    layer.setAttribute("aria-label", title);
    layer.setAttribute("data-purpose", picker.purpose);
    scanSection.hidden = choosing;
    cloneSection.hidden = choosing;
    syncField(pathInput, "path", picker.pathField);
    syncField(urlInput, "url", picker.urlField);
    syncField(nameInput, "name", picker.nameField);

    busyEl.textContent = pickerBusy(picker) ? "Working…" : "";
    errorEl.textContent = picker.error ?? "";
    errorEl.hidden = picker.error === null;
    statusEl.textContent = picker.status ?? "";
    statusEl.hidden = picker.status === null;

    scanHead.textContent =
      picker.scanned === null
        ? "PROJECTS"
        : `PROJECTS IN ${tildePath(picker.scanned.root, picker.home)}`;
    clear(scanList);
    append(scanList, scanRows(picker, request));

    clear(browseTools);
    browseTools.append(
      toolButton("Go", "go", () => request({ kind: "browse", path: pathInput.value })),
      toolButton("Up", "up", () => request({ kind: "up" }), picker.parent === null),
      choosing
        ? toolButton(
            "Choose this folder",
            "choose-here",
            () => {
              if (picker.cwd !== null) {
                store.dispatch({ type: "picker/chosen", path: picker.cwd });
              }
            },
            picker.cwd === null,
          )
        : toolButton(
            "Open this folder",
            "open-here",
            () => {
              if (picker.cwd !== null) {
                request({ kind: "open_path", path: picker.cwd });
              }
            },
            picker.cwd === null,
          ),
      toolButton(
        picker.showHidden ? "Hide hidden" : "Show hidden",
        "toggle-hidden",
        () => request({ kind: "toggle_hidden" }),
      ),
    );
    clear(browseList);
    append(browseList, browseRows(picker, request));

    clear(cloneTools);
    cloneTools.append(toolButton("Clone", "clone", () => request({ kind: "clone" })));
    const line = cloneLine(picker.clone);
    cloneLineEl.textContent = line ?? "";
    cloneLineEl.hidden = line === null;
    cloneLineEl.setAttribute(
      "data-tone",
      picker.clone?.done === null || picker.clone?.done === undefined
        ? "progress"
        : picker.clone.done.ok
          ? "ok"
          : "alert",
    );

    /** Focus lands in the path bar when the panel opens, so typing a path is
     * the first thing that works — but only then, never on a re-render. */
    if (!wasOpen) {
      wasOpen = true;
      queueMicrotask(() => {
        if (layer.isConnected && !layer.hidden) {
          pathInput.focus();
        }
      });
    }
  }

  function syncField(
    input: HTMLInputElement,
    name: "path" | "url" | "name",
    value: string,
  ): void {
    if (rendered[name] !== value) {
      rendered[name] = value;
      input.value = value;
    }
  }

  return { el: layer, update: render };
}

function field(name: string, placeholder: string, label: string): HTMLInputElement {
  return el("input", {
    class: "fd-picker__input",
    attrs: {
      type: "text",
      name,
      placeholder,
      "aria-label": label,
      autocomplete: "off",
      spellcheck: "false",
      autocapitalize: "off",
    },
  });
}

function toolButton(
  label: string,
  action: string,
  onClick: () => void,
  disabled = false,
): HTMLElement {
  const button = el("button", {
    class: "fd-picker__button",
    text: label,
    attrs: disabled
      ? { type: "button", "data-action": action, disabled: "" }
      : { type: "button", "data-action": action },
  });
  button.addEventListener("click", onClick);
  return button;
}

function scanRows(
  picker: PickerState,
  request: (intent: PickerIntent) => void,
): Child[] {
  const scan = picker.scanned;
  if (scan === null) {
    return [muted(picker.pending.scan?.waiting === true ? "Scanning…" : "")];
  }
  if (!scan.rootExists) {
    return [muted(`${tildePath(scan.root, picker.home)} does not exist yet — clone into it below.`)];
  }
  if (scan.repos.length === 0) {
    return [muted(`No repositories in ${tildePath(scan.root, picker.home)}.`)];
  }
  return scan.repos.map((repo) =>
    row(
      repo.name,
      tildePath(repo.path, picker.home),
      repo.isOpenProject,
      () => request({ kind: "open_path", path: repo.path }),
      null,
    ),
  );
}

function browseRows(
  picker: PickerState,
  request: (intent: PickerIntent) => void,
): Child[] {
  if (picker.cwd === null) {
    return [muted(picker.pending.browse?.waiting === true ? "Listing…" : "")];
  }
  /** Choosing a folder opens nothing, so no row offers Open (or says open). */
  const choosing = picker.purpose === "choose_folder";
  const rows: Child[] = picker.entries.map((entry) =>
    row(
      entry.name,
      null,
      !choosing && entry.isOpenProject,
      entry.isGitRepo && !choosing
        ? () => request({ kind: "open_path", path: entry.path })
        : null,
      () => request({ kind: "browse", path: entry.path }),
    ),
  );
  if (picker.entries.length === 0) {
    rows.push(muted("No folders here."));
  }
  if (picker.truncated) {
    rows.push(muted("Only the first folders are listed: type a path to go further."));
  }
  return rows;
}

/**
 * One repository or folder. `open` is the Open button (a git repository);
 * `enter` makes the name a link into the folder. A project already open on the
 * worker says so instead of offering to open it twice.
 */
function row(
  name: string,
  detail: string | null,
  alreadyOpen: boolean,
  open: (() => void) | null,
  enter: (() => void) | null,
): HTMLElement {
  const label =
    enter === null
      ? el("span", { class: "fd-picker__name", text: name })
      : el("button", {
          class: "fd-picker__name fd-picker__name--link",
          text: `${name}/`,
          attrs: { type: "button", "data-action": "enter" },
        });
  if (enter !== null) {
    label.addEventListener("click", enter);
  }
  const action: Child = alreadyOpen
    ? el("span", { class: "fd-picker__tag", text: "open" })
    : open === null
      ? null
      : (() => {
          const button = el("button", {
            class: "fd-picker__button fd-picker__button--open",
            text: "Open",
            attrs: { type: "button", "data-action": "open" },
          });
          button.addEventListener("click", open);
          return button;
        })();
  return el(
    "div",
    {
      class: "fd-picker__item",
      attrs: { "data-name": name, "data-open": String(alreadyOpen) },
    },
    [
      label,
      detail === null ? null : el("span", { class: "fd-picker__detail", text: detail }),
      el("span", { class: "fd-spacer" }),
      action,
    ],
  );
}

function muted(text: string): HTMLElement {
  return el("p", { class: "fd-picker__muted", text });
}
