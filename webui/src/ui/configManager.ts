import { CONFIG_FIELDS, resolveConfigRow } from "../state/config";
import type { ConfigField, ConfigOrigin, ResolvedConfigRow } from "../state/config";
import type { AppState, ConfigOutcome, ConfigState } from "../state/types";
import { clear, el, hint, hostOnlyBadge } from "./dom";
import type { Region } from "./dom";
import type { Store } from "./store";

/**
 * Artboard `1f — CONFIGURATION MANAGER` (`remote-control-ll5.6`), lines
 * 1381-1433 of `specs/design/flightdeck-web-turn2.dc.html`. Opened by the
 * palette's "Open Configuration" row (`ui/app.ts`'s `runCommand`), closed by
 * `Esc`.
 *
 * Same split as `ui/commandPalette.ts`: `state/config.ts` owns the curated
 * inventory and the origin-attribution logic; this module only ever renders
 * `state.config` and `resolveConfigRow`'s answer for each field. `Tab`/`↑↓`/
 * `c`/`s`/`Esc` are handled in `ui/app.ts`'s keydown listener, the same place
 * every other overlay's keyboard lives — this component only wires row clicks
 * (`config/select`), which is the mouse half of the same action `↑↓` drives.
 *
 * ## What is deliberately not here
 *
 * The dialog family, git commands, the palette itself and split-view
 * toggling are each a separate M2 task. Nor is a `Space`-driven "set a new
 * value" edit: SPECS §8 and the ll5.6 brief both name `Tab`/`c`/`s`/`e` as the
 * behaviour this task builds; only `c` (clear a project override) changes a
 * value, and it does so locally (staged in `state.config.edits`) until `s`
 * turns the whole staged set into one `save_config` command.
 *
 * ## Saving follows the host's `Ack`, never optimism
 *
 * Pressing `s` does not clear "Unsaved changes" or repaint origin tags as if
 * the save succeeded. It adds a `pending` entry (`config/dispatched`) and
 * everything renders exactly as it did until `command/result` arrives — a
 * real `Ack` (`applied`/`rejected`/`ignored`) or the read-only refusal — at
 * which point `renderStatus` below is the only thing that changes, and the
 * staged edits are only cleared on `applied`.
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
        hint("c", "clear override"),
        hint("Tab", "switch scope"),
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

    renderScope(state, config);
    renderRows(config);
    renderStatus(config.pending, config.lastOutcome);
    unsavedEl.hidden = Object.keys(config.edits).length === 0;
  }

  function renderScope(state: AppState, config: ConfigState): void {
    const projectName =
      state.projects.find((p) => p.id === state.selection?.projectId)?.name ??
      null;
    clear(scopeTabs);
    scopeTabs.append(
      scopeTab("global", "Global", config.scope === "global"),
      scopeTab(
        "project",
        projectName === null ? "Project" : `Project (${projectName})`,
        config.scope === "project",
      ),
    );
    pathEl.textContent =
      config.scope === "global"
        ? "~/.flightdeck/config.toml"
        : "<repo>/.flightdeck/config.toml";
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
    for (const field of CONFIG_FIELDS) {
      const resolved = resolveConfigRow(field, config.scope, config.edits);
      body.append(row(field, resolved, config.selectedKey === field.key));
      if (resolved.warning !== null) {
        body.append(noteBar(resolved.warning, "fd-config__note--inline"));
      }
    }
  }

  function row(
    field: ConfigField,
    resolved: ResolvedConfigRow,
    selected: boolean,
  ): HTMLElement {
    const cursor = el("span", {
      class: "fd-config__cursor",
      text: selected ? "▸" : "",
      attrs: { "aria-hidden": "true" },
    });
    const value = el("span", {
      class: "fd-config__value",
      text: valueText(field, resolved),
      attrs: {
        "data-kind": field.kind,
        "data-checked": String(field.kind === "toggle" && resolved.value === true),
      },
    });
    const label = el("span", { class: "fd-config__label", text: field.label });
    const origin = originCell(resolved.origin);

    const rowEl = el(
      "div",
      {
        class: "fd-config__row",
        attrs: {
          "data-selected": String(selected),
          "data-host-only": String(field.hostOnly === true),
        },
      },
      [cursor, value, label, origin],
    );
    if (field.hostOnly !== true) {
      rowEl.addEventListener("click", () => {
        store.dispatch({ type: "config/select", key: field.key });
      });
    }
    return rowEl;
  }

  function originCell(origin: ConfigOrigin | null): HTMLElement {
    if (origin === null) {
      return el("span", { class: "fd-config__origin" }, [hostOnlyBadge()]);
    }
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

function valueText(field: ConfigField, resolved: ResolvedConfigRow): string {
  if (resolved.value === null) {
    return "—";
  }
  if (field.kind === "toggle") {
    return resolved.value === true ? "[x]" : "[ ]";
  }
  if (field.kind === "choice") {
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
      return "Saved";
    case "rejected":
      return outcome.detail === null ? "Save rejected" : `Save rejected — ${outcome.detail}`;
    case "ignored":
      return outcome.detail === null ? "Save ignored" : `Save ignored — ${outcome.detail}`;
    case "read_only":
      return `Save refused — ${outcome.detail ?? "take over to drive"}`;
  }
}
