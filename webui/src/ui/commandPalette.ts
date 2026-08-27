import {
  buildCommandInventory,
  paletteColumns,
  type LabelSpan,
  type MatchedCommand,
  type PaletteCommand,
  type PaletteGroup,
} from "../state/commands";
import type { AppState, PaletteOutcome, PaletteState } from "../state/types";
import { clear, el, hostOnlyBadge } from "./dom";
import type { Region } from "./dom";

/**
 * Artboard `1d — COMMAND PALETTE`, filtered.
 *
 * §5 ("Positions the design locked in"): palette-primary, `Ctrl-g` the only
 * chord claimed — `app.ts` toggles `AppState.palette` on it and owns the rest
 * of the keyboard (typing, `↑↓`, `Tab`, `Enter`, `Esc`); this module only
 * renders `state.palette` and reports which command a click or `Enter` means.
 *
 * ## What is deliberately not here
 *
 * `D13`'s dialog family, git commands, the configuration manager and
 * split-view toggling are each a separate M2 task (`remote-control-ll5.3`
 * through `.7`) — this only ever runs a command the host can already execute
 * or D16 says must stay visible anyway (`src/state/commands.ts` has the full
 * accounting of why the inventory is short).
 *
 * ## Results follow the `Ack`, never optimism
 *
 * Running a row does not remove it, grey it out, or claim it worked. It adds
 * a `pending` entry (`palette/dispatched`) and the filter/count/rows keep
 * rendering exactly as they were until `command/result` arrives — from a real
 * `Ack` (`applied`/`rejected`/`ignored`) or the read-only refusal
 * (`ServerMsg::Error { code: "read_only" }`, D14) — at which point
 * `renderOutcome` below is the only thing that changes.
 */

export interface CommandPaletteOptions {
  /** `Enter`, or a click on a row: run this command. What happens to it next
   * is `main.ts`'s job (send the frame, dispatch `palette/dispatched` with the
   * seq the transport assigned) — this component never calls `sendCommand`
   * itself, so it stays testable with no socket. */
  readonly onRun?: (command: PaletteCommand) => void;
}

export function createCommandPalette(
  options: CommandPaletteOptions = {},
): Region {
  const countEl = el("span", { class: "fd-palette__count" });
  const textEl = el("span", { class: "fd-palette__text" });
  const status = el("div", { class: "fd-palette__status" });
  const columnEls = [
    el("div", { class: "fd-palette__column", attrs: { "data-column": "0" } }),
    el("div", { class: "fd-palette__column", attrs: { "data-column": "1" } }),
  ] as const;

  const panel = el(
    "div",
    { class: "fd-palette__panel" },
    [
      el("div", { class: "fd-palette__head" }, [
        el("span", { class: "fd-palette__title", text: "Command Palette" }),
        el("span", { class: "fd-palette__hint", text: "Esc to close" }),
      ]),
      el("div", { class: "fd-palette__filter" }, [
        el("span", {
          class: "fd-palette__prompt",
          text: ">",
          attrs: { "aria-hidden": "true" },
        }),
        textEl,
        el("span", {
          class: "fd-palette__caret",
          text: " ",
          attrs: { "aria-hidden": "true" },
        }),
        el("div", { class: "fd-spacer" }),
        countEl,
      ]),
      status,
      el("div", { class: "fd-palette__body" }, [columnEls[0], columnEls[1]]),
      el("div", { class: "fd-palette__foot" }, [
        foothint("↑↓", "move"),
        foothint("Tab", "next column"),
        foothint("Enter", "run"),
        el("div", { class: "fd-spacer" }),
        el("span", {
          class: "fd-palette__tagline",
          text: "the palette is the primary surface — every action in FlightDeck is here",
        }),
      ]),
    ],
  );

  const layer = el(
    "div",
    {
      class: "fd-palette",
      attrs: { role: "dialog", "aria-label": "Command palette" },
    },
    [panel],
  );

  function render(state: AppState): void {
    const palette = state.palette;
    layer.hidden = palette === null;
    if (palette === null) {
      return;
    }

    textEl.textContent = palette.filter;

    const inventory = buildCommandInventory(state);
    const { columns, matchedCount, totalCount } = paletteColumns(
      inventory,
      palette.filter,
    );
    countEl.textContent = `${matchedCount} of ${totalCount} commands`;

    for (const column of [0, 1] as const) {
      clear(columnEls[column]);
      const selectedRow = column === palette.column ? palette.index : -1;
      let position = 0;
      for (const group of columns[column]) {
        columnEls[column].append(groupHeading(group));
        for (const matched of group.rows) {
          const isSelected = position === selectedRow;
          columnEls[column].append(row(matched, isSelected, options.onRun));
          position += 1;
        }
      }
    }
    renderStatus(palette.pending, palette.lastOutcome);
  }

  function renderStatus(
    pending: PaletteState["pending"],
    lastOutcome: PaletteOutcome | null,
  ): void {
    clear(status);
    const running = pending.at(-1);
    if (running !== undefined) {
      status.hidden = false;
      status.setAttribute("data-outcome", "pending");
      status.append(`${running.label}: running…`);
      return;
    }
    if (lastOutcome === null) {
      status.hidden = true;
      status.removeAttribute("data-outcome");
      return;
    }
    status.hidden = false;
    status.setAttribute("data-outcome", lastOutcome.outcome);
    status.append(outcomeText(lastOutcome));
  }

  return { el: layer, update: render };
}

function outcomeText(outcome: PaletteOutcome): string {
  const { label, detail } = outcome;
  switch (outcome.outcome) {
    case "applied":
      return `${label}: done`;
    case "rejected":
      return detail === null ? `${label}: rejected` : `${label}: rejected — ${detail}`;
    case "ignored":
      return detail === null ? `${label}: ignored` : `${label}: ignored — ${detail}`;
    case "read_only":
      return `${label}: read-only — ${detail ?? "take over to drive"}`;
  }
}

function groupHeading(group: PaletteGroup): HTMLElement {
  return el("div", { class: "fd-palette__group", text: group.name });
}

function row(
  matched: MatchedCommand,
  selected: boolean,
  onRun: ((command: PaletteCommand) => void) | undefined,
): HTMLElement {
  const { command, labelSpans } = matched;
  const label = el(
    "span",
    { class: "fd-palette__label" },
    labelSpans.map((span) => labelSpan(span)),
  );
  const tag = rowTag(command, selected);
  const button = el(
    "button",
    {
      class: "fd-palette__row",
      attrs: { type: "button", "data-selected": String(selected) },
    },
    [label, tag],
  );
  button.addEventListener("click", () => onRun?.(command));
  return button;
}

function labelSpan(span: LabelSpan): HTMLElement {
  return span.matched
    ? el("span", { class: "fd-palette__match", text: span.text })
    : el("span", { text: span.text });
}

/** The row's right-hand tag: the selected row always shows the key that runs
 * it (1d), which takes priority over any tag it would otherwise show. */
function rowTag(command: PaletteCommand, selected: boolean): HTMLElement | null {
  if (selected) {
    return el("span", { class: "fd-palette__key", text: "Enter" });
  }
  if (command.hostOnly === true) {
    return hostOnlyBadge();
  }
  if (command.annotation !== undefined) {
    return el("span", { class: "fd-palette__annotation", text: command.annotation });
  }
  return null;
}

function foothint(key: string, label: string): HTMLElement {
  return el("span", { class: "fd-hint" }, [
    el("span", { class: "fd-key", text: key }),
    ` ${label}`,
  ]);
}
