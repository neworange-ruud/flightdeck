import {
  atNameStep,
  branchFieldVisible,
  cancelButton,
  decidingKeys,
  dialogOriginLabel,
  dialogStatus,
  dialogTitle,
  gateSatisfied,
  gatedKey,
  hasToggle,
  selectedChoice,
  toggleButton,
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
 *
 * **The title and that button's label move with the draft** (§6.5 R19). The
 * toggle stays a local draft — R8's reason for it is that a coalesced resync
 * mid-typing must not empty the branch field, and that is untouched — so the
 * host sends the words for *both* states and `dialogTitle` / `toggleButton`
 * pick the pair the draft is in. Before that the panel could badge itself
 * `no worktree` while its own button read `Run from base: off`, because the
 * button's words were the host's `run_on_base`, which does not flip until the
 * confirm lands.
 *
 * ## Artboard 1g's two steps are the same panel again
 *
 * 1g draws a destructive confirmation twice: `step 1 of 2` with the
 * consequences and the keyed buttons, then `step 2 of 2 — confirm` with a field
 * where the session's own name is typed back. That is `draft.step` here, and it
 * moves only when the button the *host* marked as gated is pressed
 * (`dialog.gate.key`) — pressing it sends nothing, which is what makes step 1
 * free of consequences.
 *
 * Two properties this component must not lose:
 *
 * 1. **`Esc Cancel` is never disabled, at either step.** R8: a shared dialog a
 *    remote surface can see but not dismiss is worse than not sharing it.
 * 2. **Nothing here decides what is dangerous.** The gate, its key, its expected
 *    name and its sentence are all the host's (`protocol::ConfirmGate`); this
 *    file draws them. A browser-authored list of destructive dialogs is exactly
 *    what R7 removed from the palette.
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
  onAdvance?: () => void,
): Region {
  const titleEl = el("span", { class: "fd-dialog__title" });
  const kindEl = el("span", { class: "fd-dialog__kind" });
  const originEl = el("div", { class: "fd-dialog__origin" });
  const refusalEl = el("div", { class: "fd-dialog__refusal" });
  const listEl = el("div", { class: "fd-dialog__list" });
  const gateEl = el("div", { class: "fd-dialog__gate" });
  const fieldEl = el("div", { class: "fd-dialog__field" });
  const statusEl = el("div", { class: "fd-dialog__status" });
  const actionsEl = el("div", { class: "fd-dialog__actions" });

  const panel = el("div", { class: "fd-dialog__panel" }, [
    el("div", { class: "fd-dialog__head" }, [titleEl, kindEl]),
    el("div", { class: "fd-dialog__body" }, [
      originEl,
      refusalEl,
      listEl,
      gateEl,
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
    /** 1g's `step 1 of 2` / `step 2 of 2`, as an attribute so the panel can be
     * styled — and read by a test — without parsing the header's words. */
    panel.setAttribute(
      "data-step",
      dialog.gate === null ? "" : String(dialog.draft.step),
    );

    /** The host's wording for the state the **draft** is in, not the state the
     * host is in — see the header. A dialog with no toggle has one title and
     * this is it. */
    titleEl.textContent = dialogTitle(dialog);
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
    renderGate(dialog);
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

  /**
   * 1g's step 2: the host's sentence, then the field, then the name it is
   * waiting for — drawn to the right of the caret exactly as the artboard hints
   * it. The hint is not a shortcut: it is `gate.expected` verbatim, which is the
   * same string the host will compare against, so what the reader copies and
   * what the host checks cannot drift.
   */
  function renderGate(dialog: DialogState): void {
    clear(gateEl);
    gateEl.hidden = !atNameStep(dialog);
    if (!atNameStep(dialog) || dialog.gate === null) {
      return;
    }
    const satisfied = gateSatisfied(dialog);
    gateEl.append(
      el("div", {
        class: "fd-dialog__gate-instruction",
        text: dialog.gate.instruction,
      }),
      el(
        "div",
        {
          class: "fd-dialog__input",
          attrs: { "data-satisfied": String(satisfied) },
        },
        [
          el("span", {
            class: "fd-dialog__typed",
            text: dialog.draft.confirmName,
          }),
          el("span", {
            class: "fd-dialog__caret",
            text: " ",
            attrs: { "aria-hidden": "true" },
          }),
          el("span", { class: "fd-dialog__gate-spacer" }),
          el("span", {
            class: "fd-dialog__gate-hint",
            text: dialog.gate.expected,
          }),
        ],
      ),
    );
  }

  function renderList(dialog: DialogState): void {
    clear(listEl);
    /** At step 2 the question has been read; the panel is about the name. */
    listEl.hidden = dialog.list.length === 0 || atNameStep(dialog);
    if (listEl.hidden) {
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
    if (dialog.input === null || atNameStep(dialog)) {
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
    if (atNameStep(dialog) && dialog.gate !== null) {
      /** Step 2 offers one verb and a cancel, as 1g draws it. The verb keeps
       * the label the host printed on the button at step 1 — `Abandon
       * (force)`, `Quit`, `Rebase` — rather than a word this file invented. */
      const gated = dialog.buttons.find((b) => b.key === dialog.gate?.key);
      const label = gated?.label ?? "Confirm";
      actionsEl.append(
        action(
          "danger",
          { key: "Enter", label, cancels: false },
          dialog.confirmable && gateSatisfied(dialog),
          () => options.onConfirm?.(dialog.gate?.key ?? "Enter"),
        ),
      );
      actionsEl.append(cancelAction(dialog));
      return;
    }
    for (const button of decidingKeys(dialog)) {
      /** The gated button does not decide anything at step 1: it opens step 2,
       * locally, and the host hears nothing until a name is typed. It is also
       * the one the host has marked destructive, so it wears 2g's alert hue
       * rather than the accent every other verb wears — 1g draws the red button
       * and the cancel beside it, and they must not look alike. */
      const gated = gatedKey(dialog, button.key);
      actionsEl.append(
        action(gated ? "danger" : "primary", button, dialog.confirmable, () =>
          gated ? onAdvance?.() : options.onConfirm?.(button.key),
        ),
      );
    }
    const toggle = toggleButton(dialog);
    if (toggle !== null) {
      const row = action("secondary", toggle, true, () => onToggle?.());
      row.setAttribute("data-on", String(dialog.draft.toggled));
      actionsEl.append(row);
    }
    actionsEl.append(cancelAction(dialog));
  }

  /**
   * The one cancel, on the one key that cancels every dialog on both surfaces.
   *
   * It is drawn by this component rather than taken from the host's button row
   * because it is a different *frame*: `dialog_cancel`, which is never gated and
   * never refused, where the host's `n` would travel as a `dialog_confirm` — the
   * frame artboard 1g's gate stands in front of. Rendering both put three
   * buttons on 1g's step 1, two of which cancelled, so `decidingKeys` now drops
   * the button the host marked `cancels` and this is what remains (§6.5 R19).
   *
   * The **word** is still the host's: `Cancel` is read off that button, and the
   * literal is only the fallback for a dialog the host gave no cancel at all.
   * **Always enabled, at both of 1g's steps** — dismissing a confirmation
   * cannot destroy anything, and a shared dialog a remote surface can see but
   * not dismiss would be worse than not sharing it (R8).
   */
  function cancelAction(dialog: DialogState): HTMLElement {
    const label = cancelButton(dialog)?.label ?? "Cancel";
    return action(
      "tertiary",
      { key: "Esc", label, cancels: true },
      true,
      () => options.onCancel?.(),
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
  /** 1g prints the step count where every other dialog prints its keys — it is
   * the one thing worth knowing about a dialog that has two panels. */
  if (dialog.gate !== null) {
    return dialog.draft.step === 2 ? "step 2 of 2 — confirm" : "step 1 of 2";
  }
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
