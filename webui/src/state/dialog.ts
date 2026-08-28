import type { AppState, DialogOutcome, DialogState } from "./types";

/**
 * D13's shared dialog, as pure logic (`specs/WEB_INTERFACE.md` D13, artboard
 * `1e — NEW AGENT DIALOG, BOTH STATES`).
 *
 * ## The one thing to get right
 *
 * A dialog is **app state**, not a browser overlay. It appears on both surfaces
 * because the host published it, it carries who opened it, and either surface
 * can confirm or cancel. So nothing here opens or closes a dialog: `AppState.
 * dialog` only ever changes because a `Snapshot` or a `Delta::DialogOpened` /
 * `Delta::DialogClosed` said so. `ui/dialog.ts` renders it and `ui/app.ts` binds
 * its keys; both read this module's answers.
 *
 * ## Why there is a local draft, and why that is not optimism
 *
 * 1e's form has three things the user fills in — the agent radio, the branch
 * name, and the `Tab` run-from-base toggle — and the host only learns about them
 * when the dialog is *confirmed* (`dialog_confirm` carries `list_index`, `text`
 * and `toggle`; see `src/web/protocol.rs`'s `command::DIALOG_CONFIRM`). That is
 * deliberate on the wire: a keystroke-per-character round trip would make a
 * remote form unusable, and the host synthesises the same keypresses on confirm
 * either way.
 *
 * The draft is therefore **local editing state, never claimed state**. Nothing in
 * `draft` is ever rendered as though the host had accepted it, the dialog is
 * still the host's (`id`, `title`, `list`, `buttons` all come from the wire), and
 * the confirm is settled by the host's own `Ack` like every other command
 * (`pending` / `lastOutcome`, the same shape `PaletteState` uses).
 *
 * ## What a browser is allowed to press
 *
 * `buttons` is the set the host is showing, and `choice` names one of them by
 * its key. A browser cannot reach an action the person at the desktop cannot
 * see, because the host refuses a key that is not on the open dialog.
 * `confirmable: false` is the host saying "you may cancel this one but not
 * confirm it" — today the destructive family alone
 * (`remote-control-ll5.4`) — with `refusal` carrying the sentence to show.
 *
 * The git confirmations (push / merge / rebase) were in that set until
 * `remote-control-ll5.5` and no longer are: those dialogs *are* SPECS §5's
 * confirmation, so a surface that could raise the question but not answer it
 * would leave it stranded. Nothing here changed to allow it — the host sends
 * `confirmable: true` now, and this module has never had a list of its own.
 */

/** The wire name for confirming the open dialog (`command::DIALOG_CONFIRM`). */
export const DIALOG_CONFIRM_COMMAND = "dialog_confirm";
/** The wire name for cancelling it (`command::DIALOG_CANCEL`). */
export const DIALOG_CANCEL_COMMAND = "dialog_cancel";

/** One choice row: 1e's agent radio, the folder browser's subdirectories. */
export interface DialogChoice {
  readonly label: string;
  /** The row the **host** has highlighted. Local movement lives in `draft`. */
  readonly selected: boolean;
}

/** One button the host is showing, and the key that fires it. */
export interface DialogKey {
  /** `y`, `1`, `i`, `Enter`, `Tab`, `Esc` — the desktop's own accelerator. */
  readonly key: string;
  readonly label: string;
}

/**
 * Who opened the dialog (D13).
 *
 * `label` is the host's seat label, rendered verbatim — splitting it into an
 * address and a browser name here would be parsing untrusted display text, the
 * same reasoning `ui/takeover.ts` records for the seat chip.
 */
export type DialogOrigin =
  | { readonly kind: "desktop" }
  | { readonly kind: "browser"; readonly label: string };

/** What the user has filled into 1e's form but not yet confirmed. */
export interface DialogDraft {
  /** The text field's content. Empty when the dialog has no field. */
  readonly text: string;
  /** Index into `DialogState.list`, or `null` when nothing has been moved and
   * the host's own highlight still stands. */
  readonly index: number | null;
  /** 1e's `Tab` — run from the base branch, no worktree. */
  readonly toggled: boolean;
}

/**
 * D13's origin line, as the browser words it.
 *
 * Rendered in **both** directions and never omitted, because in both directions
 * it answers the same question: a dialog you did not open has appeared, and this
 * says where it came from. The desktop's half of the same rule is
 * `dialog_origin_label` in `src/lib.rs`, which renders nothing for a dialog the
 * person reading it opened — the asymmetry is the point, since on the desktop
 * there is only ever one keyboard.
 */
export function dialogOriginLabel(origin: DialogOrigin): string {
  return origin.kind === "desktop"
    ? "opened on the desktop"
    : `opened from browser · ${origin.label}`;
}

/** The row 1e draws as `(•)`: the local move if there was one, else the host's. */
export function selectedChoice(dialog: DialogState): number {
  if (dialog.draft.index !== null) {
    return dialog.draft.index;
  }
  const hosted = dialog.list.findIndex((choice) => choice.selected);
  return hosted === -1 ? 0 : hosted;
}

/**
 * 1e's right-hand state. `run from base` hides the branch field, because there
 * is nothing to name — so a confirm that toggles it must not also send text the
 * host would ignore.
 */
export function branchFieldVisible(dialog: DialogState): boolean {
  return dialog.input !== null && !dialog.draft.toggled;
}

/** The button that `Enter` presses: the first one, which is the affirmative
 * action in every dialog the host builds. `null` when there is none. */
export function primaryKey(dialog: DialogState): DialogKey | null {
  return dialog.buttons[0] ?? null;
}

/** Whether the dialog offers a `Tab` option at all (only 1e's form does). */
export function hasToggle(dialog: DialogState): boolean {
  return dialog.buttons.some((button) => button.key === "Tab");
}

/**
 * The keyed buttons a click or a keypress may fire, in display order —
 * everything except `Esc`, which is the cancel frame's job rather than a
 * confirm's, and `Tab`, which toggles rather than decides.
 */
export function decidingKeys(dialog: DialogState): readonly DialogKey[] {
  return dialog.buttons.filter(
    (button) => button.key !== "Esc" && button.key !== "Tab",
  );
}

/**
 * The `args` for one `dialog_confirm` frame.
 *
 * `choice` is omitted when the user pressed the primary action — the host
 * defaults to it, so sending it would be spelling out the default. `text` and
 * `list_index` are omitted when the dialog has nothing to fill in, so a
 * confirmation dialog's frame is `{ dialog_id }` and nothing else.
 */
export function confirmArgs(
  dialog: DialogState,
  choice: string | null,
): Record<string, unknown> {
  const args: Record<string, unknown> = { dialog_id: dialog.id };
  if (choice !== null && choice !== primaryKey(dialog)?.key) {
    args["choice"] = choice;
  }
  if (dialog.draft.toggled) {
    args["toggle"] = true;
  }
  if (dialog.list.length > 0) {
    args["list_index"] = selectedChoice(dialog);
  }
  if (branchFieldVisible(dialog) && dialog.draft.text !== "") {
    args["text"] = dialog.draft.text;
  }
  return args;
}

/** The `args` for one `dialog_cancel` frame. */
export function cancelArgs(dialog: DialogState): Record<string, unknown> {
  return { dialog_id: dialog.id };
}

/**
 * The line the panel shows about the command it sent, or `null` before it sent
 * one. Same three-state shape as the palette's own status: in flight, then the
 * host's word on it, never a guess.
 */
export function dialogStatus(
  dialog: DialogState,
): { readonly tone: string; readonly text: string } | null {
  if (dialog.pending.length > 0) {
    return { tone: "pending", text: "waiting for the host…" };
  }
  const last = dialog.lastOutcome;
  if (last === null) {
    return null;
  }
  return {
    tone: last.outcome,
    text: last.detail ?? outcomeSentence(last.outcome),
  };
}

function outcomeSentence(outcome: DialogOutcome["outcome"]): string {
  switch (outcome) {
    case "applied":
      return "the host applied it";
    case "rejected":
      return "the host refused it";
    case "ignored":
      return "the host was already past it";
    case "read_only":
      return "this tab is watching read-only";
  }
}

/** Whether the dialog overlay is up. Exported so `ui/app.ts` can order the
 * overlays' keyboard claims without reaching into the shape. */
export function dialogOpen(state: AppState): boolean {
  return state.dialog !== null;
}
