import { resolveConfigRow, stagedCount } from "../state/config";
import type { ConfigOrigin, ResolvedConfigRow } from "../state/config";
import type { AppState, ConfigOutcome, ConfigState } from "../state/types";
import { clear, el, hint, hostOnlyBadge } from "./dom";
import type { Region } from "./dom";
import type { Store } from "./store";

/**
 * Artboard `1f — CONFIGURATION MANAGER` (`remote-control-ll5.6`, rebuilt on
 * host state in `remote-control-1p22`), lines 611-663 of
 * `specs/design/flightdeck-web-turn1.dc.html`. Opened by the palette's
 * "Open Configuration" row — which is *sent to the host*, and the panel appears
 * when `ServerMsg::Configuration` answers it — and closed by `Esc`.
 *
 * Same split as `ui/commandPalette.ts`: `state/config.ts` owns the shapes and
 * the staged-edit arithmetic; this module only ever renders `state.config.doc`
 * and `resolveConfigRow`'s answer for each row. `Space`/`Tab`/`↑↓`/`c`/`s`/`Esc`
 * are handled in `ui/app.ts`'s keydown listener, the same place every other
 * overlay's keyboard lives — this component only wires row clicks
 * (`config/select`), which is the mouse half of the action `↑↓` drives.
 *
 * ## Every row here is the host's
 *
 * There is no field list in this file and none in `state/config.ts`. The rows,
 * their labels, their TOML keys, their values, a choice's options and all three
 * origin tags arrive on the wire, resolved by the very `ConfigManager` the
 * desktop's own overlay is drawn from (§6.5 R22). What this module adds is 1f's
 * *presentation* of them — `[x]`, `‹bright›`, `(set here)` — plus two notes the
 * design owns and the host has no opinion about: the relay's rate limit, and
 * D5's caution when the bind address is not loopback.
 *
 * ## Saving follows the host's `Ack`, never optimism
 *
 * Pressing `s` does not clear "Unsaved changes" or repaint origin tags as if
 * the save succeeded. It adds a `pending` entry (`config/dispatched`) and
 * everything renders exactly as it did until `command/result` arrives — a real
 * `Ack` (`applied`/`rejected`/`ignored`) or the read-only refusal — at which
 * point the staged edits are cleared only on `applied`, and the *rows* are
 * repainted only by the `configuration` frame the host sends beside it.
 */
export function createConfigManager(store: Store): Region {
  const scopeTabs = el("span", { class: "fd-config__scopes" });
  const pathEl = el("span", { class: "fd-config__path" });
  const unsavedEl = el("span", { class: "fd-config__unsaved", text: "Unsaved changes" });
  const status = el("div", { class: "fd-config__status" });
  const body = el("div", { class: "fd-config__body" });

  const panel = el(
    "div",
    { class: "fd-config__panel" },
    [
      el("div", { class: "fd-config__head" }, [
        el("span", { class: "fd-config__title", text: "Configuration" }),
        el("span", { class: "fd-config__hint", text: "Esc to close" }),
      ]),
      el("div", { class: "fd-config__scope" }, [
        scopeTabs,
        el("span", { class: "fd-config__path-label", text: "Editing: " }, [
          pathEl,
        ]),
        el("div", { class: "fd-spacer" }),
        unsavedEl,
      ]),
      status,
      el(
        "div",
        { class: "fd-config__columns", attrs: { "aria-hidden": "true" } },
        [
          el("span"),
          el("span", { text: "value" }),
          el("span", { text: "setting" }),
          el("span", { text: "origin" }),
        ],
      ),
      body,
      noteBar(
        "the default relay is rate-restricted — self-host for continuous browser control.",
      ),
      el("div", { class: "fd-config__foot" }, [
        hint("↑↓", "move"),
        hint("Space", "toggle / edit"),
        hint("Tab", "switch scope"),
        hint("c", "clear override"),
        hint("s", "save"),
        el("span", { class: "fd-config__editor-hint" }, [
          el("span", { class: "fd-key", text: "e" }),
          " edit in $EDITOR ",
          hostOnlyBadge(),
        ]),
        hint("Esc", "close"),
      ]),
    ],
  );

  const layer = el(
    "div",
    {
      class: "fd-config",
      attrs: { role: "dialog", "aria-label": "Configuration" },
    },
    [panel],
  );

  function render(state: AppState): void {
    const config = state.config;
    layer.hidden = config === null;
    if (config === null) {
      return;
    }

    renderScope(config);
    renderRows(config);
    renderStatus(config.pending, config.lastOutcome);
    unsavedEl.hidden = stagedCount(config.edits) === 0;
  }

  /**
   * The scope tabs and the file being edited, both from the host's answer —
   * the project's name included, so the tab says which project the override
   * file belongs to rather than the browser guessing from the selection.
   */
  function renderScope(config: ConfigState): void {
    clear(scopeTabs);
    scopeTabs.append(
      scopeTab("global", "Global", config.scope === "global"),
      scopeTab(
        "project",
        `Project (${config.doc.projectName})`,
        config.scope === "project",
      ),
    );
    pathEl.textContent =
      config.scope === "global"
        ? (config.doc.globalPath ??
          "no home directory on the host — there is no global base to edit")
        : config.doc.projectPath;
  }

  function scopeTab(
    scope: ConfigState["scope"],
    label: string,
    active: boolean,
  ): HTMLElement {
    const tab = el("button", {
      class: "fd-config__scope-tab",
      text: label,
      attrs: { type: "button", "data-active": String(active) },
    });
    tab.addEventListener("click", () => {
      const current = store.getState().config;
      if (current !== null && current.scope !== scope) {
        store.dispatch({ type: "config/scope" });
      }
    });
    return tab;
  }

  function renderRows(config: ConfigState): void {
    clear(body);
    for (const row of config.doc.rows[config.scope]) {
      const resolved = resolveConfigRow(row, config.scope, config.edits);
      const selected = config.selectedKey === row.key;
      const editing = selected ? config.editing : null;
      body.append(rowEl(resolved, selected, editing));
      if (resolved.warning !== null) {
        body.append(noteBar(resolved.warning, "fd-config__note--inline"));
      }
    }
  }

  function rowEl(
    resolved: ResolvedConfigRow,
    selected: boolean,
    editing: string | null,
  ): HTMLElement {
    const cursor = el("span", {
      class: "fd-config__cursor",
      text: selected ? "▸" : "",
      attrs: { "aria-hidden": "true" },
    });
    const value = el("span", {
      class: "fd-config__value",
      text: valueText(resolved, editing),
      attrs: {
        "data-kind": resolved.row.kind,
        "data-checked": String(
          resolved.row.kind === "bool" && resolved.value === true,
        ),
        "data-editing": String(editing !== null),
      },
    });
    const label = el("span", {
      class: "fd-config__label",
      text: resolved.row.label,
    });

    const el_ = el(
      "div",
      {
        class: "fd-config__row",
        attrs: {
          "data-selected": String(selected),
          "data-staged": String(resolved.staged),
          "data-key": resolved.row.key,
        },
      },
      [cursor, value, label, originCell(resolved.origin)],
    );
    el_.addEventListener("click", () => {
      store.dispatch({ type: "config/select", key: resolved.row.key });
    });
    return el_;
  }

  function originCell(origin: ConfigOrigin): HTMLElement {
    return el("span", {
      class: "fd-config__origin",
      text: originText(origin),
      attrs: { "data-origin": origin },
    });
  }

  function renderStatus(
    pending: ConfigState["pending"],
    lastOutcome: ConfigOutcome | null,
  ): void {
    clear(status);
    if (pending.length > 0) {
      status.hidden = false;
      status.setAttribute("data-outcome", "pending");
      status.append("Saving…");
      return;
    }
    if (lastOutcome === null) {
      status.hidden = true;
      status.removeAttribute("data-outcome");
      return;
    }
    status.hidden = false;
    status.setAttribute("data-outcome", lastOutcome.outcome);
    status.append(statusText(lastOutcome));
  }

  return { el: layer, update: render };
}

/** The panel's static relay note and a field's D5 routable-bind warning share
 * one visual language — both are a caution, not an error — so both are built
 * from the same shape. `modifier` only changes layout (inline vs. footer). */
function noteBar(text: string, modifier?: string): HTMLElement {
  return el(
    "div",
    { class: modifier === undefined ? "fd-config__note" : `fd-config__note ${modifier}` },
    [el("span", { class: "fd-config__note-label", text: "note" }), ` ${text}`],
  );
}

/**
 * 1f's value column. `editing` is the in-progress buffer for an open inline
 * edit, drawn with the same block cursor the desktop's overlay appends — the
 * two surfaces show a half-typed relay URL the same way.
 */
function valueText(resolved: ResolvedConfigRow, editing: string | null): string {
  if (editing !== null) {
    return `${editing}█`;
  }
  if (resolved.row.kind === "bool") {
    return resolved.value === true ? "[x]" : "[ ]";
  }
  if (resolved.row.kind === "choice") {
    return `‹${String(resolved.value)}›`;
  }
  return String(resolved.value);
}

function originText(origin: ConfigOrigin): string {
  switch (origin) {
    case "set_here":
      return "(set here)";
    case "global":
      return "(global)";
    case "default":
      return "(default)";
  }
}

function statusText(outcome: ConfigOutcome): string {
  switch (outcome.outcome) {
    case "applied":
      /** The host's own word (`Saved.`) when it sent one; the ack for a plain
       * open carries no detail, and there is nothing to report about it. */
      return outcome.detail ?? "Saved";
    case "rejected":
      return outcome.detail === null ? "Save rejected" : `Save rejected — ${outcome.detail}`;
    case "ignored":
      return outcome.detail === null ? "Save ignored" : `Save ignored — ${outcome.detail}`;
    case "read_only":
      return `Save refused — ${outcome.detail ?? "take over to drive"}`;
  }
}
