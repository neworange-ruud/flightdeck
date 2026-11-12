import {
  allSelected,
  downloadRows,
  handedOffLine,
  isListing,
  MULTIPLE_HINT,
  selectedBytes,
} from "../state/download";
import type { DownloadIntent, DownloadRow, DownloadState } from "../state/download";
import { tildePath } from "../state/picker";
import type { AppState } from "../state/types";
import { humanSize } from "../state/upload";
import { append, clear, el, hint } from "./dom";
import type { Child, Region } from "./dom";
import type { Store } from "./store";
import { button, section } from "./transferParts";

/**
 * The Transfer files panel's Download tab (v9, `remote-control-pz6d`).
 *
 * ```text
 * note line (a refusal — the worker's words verbatim)
 * FOLDER   ~/src/my-repo                         Up  Home  Show hidden
 *          ▸ docs/
 *          ▸ src/
 *          ☑ notes.txt ................................ 1.2 KB
 *          ☐ data.bin ................................. 300 KB
 * ↑↓ move  Enter open  Space select     Select all   2 files, 1.5 MB   Download
 * ```
 *
 * then *requested* is the same list, locked, with Cancel; *handed off* says
 * how many files went to the browser's downloads, with Download more and
 * Close; *cancelled* says so, with the same two.
 *
 * Everything it shows is `state.download`, and every click is an action —
 * `main.ts` sends the `browse_dir` and `begin_download` they mean, and hands
 * the tickets to the browser. **The list is built once** and only its rows
 * are rebuilt, so it keeps focus while ↑↓ walk it: it is a `listbox`, its
 * rows `option`s, the cursor its `aria-activedescendant`.
 */
export interface DownloadViewRegion extends Region {
  /** Put focus on the list, so the keyboard works at once. */
  focus(): void;
}

export function createDownloadView(store: Store): DownloadViewRegion {
  const intent = (wanted: DownloadIntent): void =>
    store.dispatch({ type: "download/intent", intent: wanted });

  const noteEl = el("p", { class: "fd-upload__note", attrs: { role: "alert" } });
  const pathEl = el("span", {
    class: "fd-picker__name fd-download__path",
    attrs: { "data-path": "" },
  });
  const busyEl = el("span", { class: "fd-picker__busy" });
  const tools = el("div", { class: "fd-picker__tools" });
  const list = el("ul", {
    class: "fd-upload__list fd-download__list",
    attrs: {
      role: "listbox",
      tabindex: "0",
      "aria-multiselectable": "true",
      "aria-label": "Folders and files on the worker",
      "data-list": "download",
    },
  });
  const listNote = el("div", { class: "fd-download__notes" });
  const browse = el("section", { class: "fd-picker__section", attrs: { "data-section": "browse" } }, [
    el("div", { class: "fd-picker__section-head" }, [
      el("span", { class: "fd-picker__section-title", text: "FOLDER" }),
    ]),
    el("div", { class: "fd-picker__row" }, [pathEl, busyEl, el("span", { class: "fd-spacer" }), tools]),
    list,
    listNote,
  ]);
  const summary = el("div", { class: "fd-download__summary" });
  const foot = el("div", { class: "fd-upload__foot" });
  const root = el("div", { class: "fd-download" }, [noteEl, browse, summary, foot]);

  list.addEventListener("keydown", (event: KeyboardEvent) => {
    const download = store.getState().download;
    if (download === null || event.altKey || event.ctrlKey || event.metaKey) {
      return;
    }
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        store.dispatch({ type: "download/move", delta: 1 });
        return;
      case "ArrowUp":
        event.preventDefault();
        store.dispatch({ type: "download/move", delta: -1 });
        return;
      case "Enter": {
        event.preventDefault();
        const row = downloadRows(download)[download.cursor];
        if (row !== undefined) {
          activate(row);
        }
        return;
      }
      case " ":
        event.preventDefault();
        store.dispatch({ type: "download/toggleCursor" });
        return;
      default:
        return;
    }
  });

  /** A folder opens; a file is ticked or unticked. */
  function activate(row: DownloadRow): void {
    if (row.kind === "folder") {
      intent({ kind: "browse", path: row.entry.path });
    } else {
      store.dispatch({ type: "download/toggle", name: row.file.name });
    }
  }

  function render(state: AppState): void {
    const download = state.download;
    if (download === null) {
      return;
    }
    root.setAttribute("data-phase", download.phase.kind);
    noteEl.textContent = download.note ?? "";
    noteEl.hidden = download.note === null;

    const phase = download.phase;
    const listed = phase.kind === "browsing" || phase.kind === "requested";
    const locked = phase.kind !== "browsing";
    browse.hidden = !listed;
    summary.hidden = listed;

    pathEl.textContent = tildePath(download.cwd ?? download.start, download.home);
    busyEl.textContent = isListing(download) ? "Listing…" : "";
    clear(tools);
    tools.append(
      button("Up", "up", () => intent({ kind: "up" }), locked || download.parent === null),
      button("Home", "home", () => intent({ kind: "home" }), locked),
      button(
        download.showHidden ? "Hide hidden" : "Show hidden",
        "toggle-hidden",
        () => intent({ kind: "toggle_hidden" }),
        locked,
      ),
    );

    clear(list);
    const rows = downloadRows(download);
    rows.forEach((row, index) => list.append(rowEl(download, row, index, locked)));
    const cursorRow = rows.length === 0 ? null : list.children[download.cursor] ?? null;
    if (cursorRow === null) {
      list.removeAttribute("aria-activedescendant");
    } else {
      list.setAttribute("aria-activedescendant", cursorRow.id);
      (cursorRow as HTMLElement & { scrollIntoView?: (o: object) => void }).scrollIntoView?.({
        block: "nearest",
      });
    }
    clear(listNote);
    if (download.cwd !== null && rows.length === 0) {
      listNote.append(muted("No folders or files here."));
    }
    if (download.truncated) {
      listNote.append(muted("Only the first entries are listed."));
    }

    clear(summary);
    clear(foot);
    append(summary, summaryView(download));
    append(foot, footView(download));
  }

  function rowEl(
    download: DownloadState,
    row: DownloadRow,
    index: number,
    locked: boolean,
  ): HTMLElement {
    const cursor = index === download.cursor;
    if (row.kind === "folder") {
      const name = el("button", {
        class: "fd-picker__name fd-picker__name--link",
        text: `${row.entry.name}/`,
        attrs: { type: "button", tabindex: "-1", "data-action": "enter" },
      });
      if (locked) {
        name.setAttribute("disabled", "");
      }
      const item = el(
        "li",
        {
          class: "fd-picker__item fd-download__row",
          attrs: {
            id: `fd-download-row-${index}`,
            role: "option",
            "aria-selected": "false",
            "data-kind": "folder",
            "data-name": row.entry.name,
            "data-cursor": String(cursor),
          },
        },
        [el("span", { class: "fd-download__mark", text: "▸", attrs: { "aria-hidden": "true" } }), name],
      );
      item.addEventListener("click", () => {
        /** The row is rebuilt under the click, so the list keeps focus. */
        list.focus();
        store.dispatch({ type: "download/cursor", index });
        if (!locked) {
          activate(row);
        }
      });
      return item;
    }
    const ticked = download.selected.includes(row.file.name);
    const box = el("input", {
      class: "fd-download__check",
      attrs: {
        type: "checkbox",
        tabindex: "-1",
        "aria-label": `Select ${row.file.name}`,
        "data-action": "toggle",
      },
    });
    box.checked = ticked;
    box.disabled = locked;
    const item = el(
      "li",
      {
        class: "fd-picker__item fd-download__row",
        attrs: {
          id: `fd-download-row-${index}`,
          role: "option",
          "aria-selected": String(ticked),
          "data-kind": "file",
          "data-name": row.file.name,
          "data-cursor": String(cursor),
        },
      },
      [
        box,
        el("span", { class: "fd-picker__name", text: row.file.name }),
        el("span", { class: "fd-spacer" }),
        el("span", { class: "fd-picker__detail", text: humanSize(row.file.size) }),
      ],
    );
    item.addEventListener("click", (event: MouseEvent) => {
      list.focus();
      store.dispatch({ type: "download/cursor", index });
      if (locked) {
        event.preventDefault();
        return;
      }
      store.dispatch({ type: "download/toggle", name: row.file.name });
    });
    return item;
  }

  function summaryView(download: DownloadState): Child[] {
    const phase = download.phase;
    if (phase.kind === "handed_off") {
      const count = phase.names.length;
      return [
        section("DOWNLOADING", [
          el("p", {
            class: "fd-upload__line",
            attrs: { "data-summary": "" },
            text: handedOffLine(count),
          }),
          el(
            "ul",
            { class: "fd-upload__list", attrs: { "data-list": "handed-off" } },
            phase.names.map((name) =>
              el("li", { class: "fd-picker__item", attrs: { "data-name": name } }, [
                el("span", { class: "fd-glyph fd-tone-ok", text: "↓", attrs: { "aria-hidden": "true" } }),
                el("span", { class: "fd-picker__name", text: name }),
              ]),
            ),
          ),
          el("p", {
            class: "fd-picker__muted",
            text:
              count > 1
                ? `Your browser shows their progress. ${MULTIPLE_HINT}`
                : "Your browser shows its progress.",
          }),
        ]),
      ];
    }
    if (phase.kind === "cancelled") {
      return [
        section("CANCELLED", [
          el("p", {
            class: "fd-upload__line",
            attrs: { "data-summary": "" },
            text: "Download cancelled. Nothing was sent to your browser.",
          }),
        ]),
      ];
    }
    return [];
  }

  function footView(download: DownloadState): Child[] {
    const phase = download.phase;
    switch (phase.kind) {
      case "browsing":
      case "requested": {
        const asking = phase.kind === "requested";
        const count = download.selected.length;
        return [
          hint("Space", "select"),
          button(
            allSelected(download) ? "Select none" : "Select all",
            "select-all",
            () => store.dispatch({ type: "download/selectAll" }),
            asking || download.files.length === 0,
          ),
          el("span", { class: "fd-spacer" }),
          el("span", {
            class: "fd-picker__detail",
            attrs: { "data-total": "" },
            text:
              count === 0
                ? ""
                : `${count === 1 ? "1 file" : `${count} files`}, ${humanSize(selectedBytes(download))}`,
          }),
          asking
            ? button("Cancel", "cancel", () => store.dispatch({ type: "download/cancel" }))
            : null,
          button(
            asking ? "Asking the worker…" : "Download",
            "download",
            () => store.dispatch({ type: "download/submit" }),
            asking || count === 0,
            true,
          ),
        ];
      }
      case "granted":
        return [el("span", { class: "fd-spacer" })];
      case "handed_off":
      case "cancelled":
        return [
          el("span", { class: "fd-spacer" }),
          button("Download more", "download-more", () =>
            store.dispatch({ type: "download/startOver" }),
          ),
          button(
            "Close",
            "close-done",
            () => store.dispatch({ type: "transfer/close" }),
            false,
            true,
          ),
        ];
    }
  }

  function focus(): void {
    const download = store.getState().download;
    const listed =
      download !== null &&
      (download.phase.kind === "browsing" || download.phase.kind === "requested");
    if (listed) {
      list.focus();
    } else {
      root.querySelector<HTMLElement>('[data-primary="true"]:not([disabled])')?.focus();
    }
  }

  return { el: root, update: render, focus };
}

function muted(text: string): HTMLElement {
  return el("p", { class: "fd-picker__muted", text });
}
