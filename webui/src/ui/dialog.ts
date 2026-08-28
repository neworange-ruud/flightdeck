import {
  branchFieldVisible,
  decidingKeys,
  dialogOriginLabel,
  dialogStatus,
  hasToggle,
  selectedChoice,
} from "../state/dialog";
import type { DialogKey } from "../state/dialog";
import type { AppState, DialogState } from "../state/types";
import { clear, el, hint } from "./dom";
import type { Region } from "./dom";

/**
 * Artboard `1e — NEW AGENT DIALOG, BOTH STATES` (lines 1322-1380 of
 * `specs/design/flightdeck-web-turn2.dc.html`), and through it every dialog:
 * 1d says *"same shell for every dialog: titled accent frame, keyed buttons,
 * consequences listed before the verb"*, and the host sends that shell
 * (`protocol::DialogBody`) rather than one payload per dialog kind.
 *
 * ## Why this overlay is not like the others
 *
 * `accessScreen`, `takeover`, `commandPalette` and `configManager` are all
 * *this browser's* overlays: something local opens them and `Esc` closes them.
 * A dialog is **app state** (D13). It is on screen because the host published
 * it; `Esc` does not close it locally, it sends `dialog_cancel` and the host
 * closes it on both surfaces. So there is no `dialog/close` action, and nothing
 * here hides the panel except the host taking the dialog away.
 *
 * ## The origin line, and why it is always drawn
 *
 * D13's accepted cost is a modal you did not ask for, and the origin label is
 * what makes that acceptable. The browser draws it in both directions —
 * `opened on the desktop` or `opened from browser · 192.168.2.20` — because in
 * both directions it answers the question the modal raises. `--fd-elsewhere` is
 * 2g's "another actor acted" hue, shared with drift, App mode and the takeover
 * panel, which is exactly the fact being reported.
 *
 * ## 1e's two states are one panel and one local flag
 *
 * Left-hand state: the agent radio, the branch field, `Enter Create` /
 * `Tab Run from base: off` / `Esc Cancel`. Right-hand state: `Tab` pressed, the
 * branch field gone ("there is nothing to name"), the frame in `--fd-focus`
 * instead of `--fd-accent`. That is `state.dialog.draft.toggled`, rendered as
 * `data-toggled` on the panel — one panel, not two components, because it is
 * one dialog in two states and the artboard draws them side by side only to
 * show both at once.
 */

export interface DialogOptions {
  /** `Enter`, or a click on a keyed button: send `dialog_confirm`. `key` is the
   * button pressed, so a multi-choice dialog (`1`/`2`/`3`, `i`/`w`/`b`/`d`) can
   * name which one. Sending the frame and finding out what happened to it are
   * `main.ts`'s job, exactly as `onRunCommand` is for the palette. */
  readonly onConfirm?: (key: string) => void;
  /** `Esc`, or a click on `Cancel`: send `dialog_cancel`. Always offered, even
   * for a dialog this build will not confirm — dismissing a confirmation cannot
   * destroy anything, and a shared dialog a remote surface can see but not
   * dismiss would be worse than not sharing it. */
  readonly onCancel?: () => void;
}

export function createDialog(
  options: DialogOptions = {},
  onChoose?: (index: number) => void,
  onToggle?: () => void,
): Region {
  const titleEl = el("span", { class: "fd-dialog__title" });
  const kindEl = el("span", { class: "fd-dialog__kind" });
  const originEl = el("div", { class: "fd-dialog__origin" });
  const refusalEl = el("div", { class: "fd-dialog__refusal" });
  const listEl = el("div", { class: "fd-dialog__list" });
  const fieldEl = el("div", { class: "fd-dialog__field" });
  const statusEl = el("div", { class: "fd-dialog__status" });
  const actionsEl = el("div", { class: "fd-dialog__actions" });

  const panel = el("div", { class: "fd-dialog__panel" }, [
    el("div", { class: "fd-dialog__head" }, [titleEl, kindEl]),
    el("div", { class: "fd-dialog__body" }, [
      originEl,
      refusalEl,
      listEl,
      fieldEl,
      statusEl,
      actionsEl,
    ]),
  ]);

  const layer = el(
    "div",
    {
      class: "fd-dialog",
      attrs: { role: "dialog", "aria-label": "FlightDeck dialog" },
    },
    [panel],
  );

  function render(state: AppState): void {
    const dialog = state.dialog;
    layer.hidden = dialog === null;
    if (dialog === null) {
      return;
    }

    panel.setAttribute("data-kind", dialog.kind);
    panel.setAttribute("data-toggled", String(dialog.draft.toggled));
    panel.setAttribute("data-confirmable", String(dialog.confirmable));

    titleEl.textContent = dialog.title;
    /** 1e's right-hand header reads `no worktree` when run-from-base is on;
     * every other dialog gets the keys it is waiting for. */
    kindEl.textContent = dialog.draft.toggled
      ? "no worktree"
      : keyHintFor(dialog);

    /** D13: always drawn, never conditional. */
    originEl.textContent = dialogOriginLabel(dialog.origin);
    originEl.setAttribute("data-origin", dialog.origin.kind);

    renderRefusal(dialog);
    renderList(dialog);
    renderField(dialog);
    renderStatus(dialog);
    renderActions(dialog);
  }

  /** The host's own sentence for a dialog a browser may see and cancel but not
   * confirm. Shown rather than hidden: a disabled `Create` with no explanation
   * is the failure mode this replaces. */
  function renderRefusal(dialog: DialogState): void {
    const refusal = dialog.confirmable ? null : dialog.refusal;
    refusalEl.hidden = refusal === null;
    refusalEl.textContent = refusal ?? "";
  }

  function renderList(dialog: DialogState): void {
    clear(listEl);
    listEl.hidden = dialog.list.length === 0;
    if (dialog.list.length === 0) {
      return;
    }
    const selected = selectedChoice(dialog);
    dialog.list.forEach((choice, index) => {
      const row = el(
        "button",
        {
          class: "fd-dialog__choice",
          attrs: {
            type: "button",
            "data-selected": String(index === selected),
          },
        },
        [
          el("span", {
            class: "fd-dialog__radio",
            text: index === selected ? "(•)" : "( )",
          }),
          el("span", { class: "fd-dialog__choice-label", text: choice.label }),
        ],
      );
      row.addEventListener("click", () => onChoose?.(index));
      listEl.append(row);
    });
  }

  /**
   * 1e's branch field, and its right-hand replacement.
   *
   * When run-from-base is on the field is *gone* and the artboard's own
   * sentence takes its place — `branch field hidden — there is nothing to name`
   * — rather than a disabled input, which would leave the user wondering
   * whether the text they typed is still going to be used.
   */
  function renderField(dialog: DialogState): void {
    clear(fieldEl);
    if (dialog.input === null) {
      fieldEl.hidden = true;
      return;
    }
    fieldEl.hidden = false;
    if (!branchFieldVisible(dialog)) {
      fieldEl.append(
        el("div", {
          class: "fd-dialog__note",
          text: "branch field hidden — there is nothing to name",
        }),
      );
      return;
    }
    fieldEl.append(
      el("div", { class: "fd-dialog__field-label", text: "Branch" }),
      el("div", { class: "fd-dialog__input" }, [
        el("span", { class: "fd-dialog__typed", text: dialog.draft.text }),
        el("span", {
          class: "fd-dialog__caret",
          text: " ",
          attrs: { "aria-hidden": "true" },
        }),
      ]),
    );
  }

  /** What the host said about the answer this tab sent, never a guess. */
  function renderStatus(dialog: DialogState): void {
    const status = dialogStatus(dialog);
    statusEl.hidden = status === null;
    if (status === null) {
      statusEl.removeAttribute("data-outcome");
      statusEl.textContent = "";
      return;
    }
    statusEl.setAttribute("data-outcome", status.tone);
    statusEl.textContent = status.text;
  }

  function renderActions(dialog: DialogState): void {
    clear(actionsEl);
    for (const button of decidingKeys(dialog)) {
      actionsEl.append(
        action(
          "primary",
          button,
          dialog.confirmable,
          () => options.onConfirm?.(button.key),
        ),
      );
    }
    if (hasToggle(dialog)) {
      const toggle = dialog.buttons.find((b) => b.key === "Tab");
      if (toggle !== undefined) {
        const row = action("secondary", toggle, true, () => onToggle?.());
        row.setAttribute("data-on", String(dialog.draft.toggled));
        actionsEl.append(row);
      }
    }
    /** `Esc Cancel` is added by this component rather than read off the host's
     * buttons: not every dialog lists one (the y/n confirmations use `n`), and
     * D13 gives every dialog a cancel from either surface. */
    actionsEl.append(
      action("tertiary", { key: "Esc", label: "Cancel" }, true, () =>
        options.onCancel?.(),
      ),
    );
  }

  return { el: layer, update: render };
}

function action(
  rank: string,
  button: DialogKey,
  enabled: boolean,
  handler: () => void,
): HTMLElement {
  const node = el(
    "button",
    {
      class: `fd-dialog__action fd-dialog__action--${rank}`,
      attrs: {
        type: "button",
        "data-key": button.key,
        ...(enabled ? {} : { disabled: "disabled" }),
      },
    },
    [el("span", { class: "fd-key-box", text: button.key }), button.label],
  );
  node.addEventListener("click", () => {
    if (enabled) {
      handler();
    }
  });
  return node;
}

/** The header's right-hand hint: the keys this dialog is waiting for, in 1e's
 * own words where it has them. */
function keyHintFor(dialog: DialogState): string {
  const parts: string[] = [];
  if (dialog.list.length > 1) {
    parts.push("↑/↓ choose");
  }
  if (dialog.input !== null) {
    parts.push("type to name");
  }
  if (hasToggle(dialog)) {
    parts.push("Tab = run from base");
  }
  return parts.length === 0 ? "Enter or Esc" : parts.join(" · ");
}

/** 1d/1e's footer, exported so the panel and any test agree on the wording. */
export function dialogFooterHints(): readonly HTMLElement[] {
  return [hint("Enter", "confirm"), hint("Esc", "cancel")];
}
