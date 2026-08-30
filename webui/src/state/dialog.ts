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
 * name, and the selected target. The host learns the target immediately on
 * `Tab`, and learns the radio and text
 * values when the dialog is *confirmed* (`dialog_confirm` carries `list_index`
 * and `text`; see `src/web/protocol.rs`'s `command::DIALOG_CONFIRM`). That is
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
 * confirm it", with `refusal` carrying the sentence to show. The destructive
 * family was that set until `remote-control-ll5.4`, and the git confirmations
 * until `.5`; today it is only a gate the host cannot resolve (the session it
 * asked about is gone). Nothing here changed to allow either — the host sends
 * `confirmable: true` now, and this module has never had a list of its own.
 *
 * ## Artboard 1g's second step, and why it is not a browser-side rule
 *
 * A destructive answer from a browser takes two steps: the consequences and the
 * keyed buttons, then a field where the session's — or the project's — own name
 * is typed back (`gate`, `ConfirmGate`). Every part of that comes off the wire:
 * which button is gated, what must be typed, and the sentence saying why. The
 * browser contributes the local `draft.step` and `draft.confirmName`, which are
 * a reading position and a keystroke buffer — never a claim.
 *
 * The host checks the same name against the same expectation before it feeds a
 * single key into its prompt, so `gateSatisfied` is an affordance, not the
 * enforcement. See `specs/WEB_INTERFACE.md` §6.5 R13.
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
  /** Whether this button **dismisses** the dialog rather than deciding it
   * (`protocol::DialogKey::cancels`, §6.5 R19). The host's word, because the
   * dialogs do not agree on a cancel key — `n` in the close confirmations, `c`
   * in the push confirmation, `Esc` in the forms — and reading the labels to
   * find out would be the browser authoring a fact the host never sent. */
  readonly cancels: boolean;
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
  /** 1g step 2's field: the name typed back. Empty until the user types, and
   * never pre-filled from `gate.expected` — a gate that fills itself in is a
   * button with extra steps. */
  readonly confirmName: string;
  /** Which of 1g's two panels is showing. `1` is the consequences and the keyed
   * buttons; `2` is the name field, reached only by pressing the gated button
   * on a gated dialog. Local, because it is a reading position rather than a
   * decision: the host is told nothing until the confirm carries the name. */
  readonly step: 1 | 2;
}

/**
 * Artboard 1g's step 2, exactly as the host published it (`protocol::
 * ConfirmGate`).
 *
 * `key` is the button it guards — every *other* button on the same dialog, and
 * cancelling, stays one press away. `expected` is what must be typed back, and
 * it is host-sent because 1g draws it as the field's own hint: the gate buys
 * deliberateness, not secrecy.
 */
export interface ConfirmGate {
  readonly key: string;
  readonly expected: string;
  readonly instruction: string;
}

/**
 * Whether pressing `key` on this dialog is the answer 1g's second step guards.
 *
 * The browser asks the host's own field rather than its own opinion about which
 * commands are dangerous (R7 as amended by ll5.12: nothing about a row is
 * authored here). A dialog with no gate answers `false` for every key.
 */
export function gatedKey(dialog: DialogState, key: string): boolean {
  return dialog.gate !== null && dialog.gate.key === key;
}

/** Whether the name field is showing — 1g's second panel. */
export function atNameStep(dialog: DialogState): boolean {
  return dialog.gate !== null && dialog.draft.step === 2;
}

/**
 * Whether what has been typed is the name the host will accept.
 *
 * **Exact**, and deliberately so: no trim, no case fold, no normalisation. The
 * host compares the same two strings the same way (`apply_web_dialog` in
 * `src/lib.rs`), so a browser that accepted `Task ` would be enabling a button
 * the host is about to refuse — which is worse than a disabled one, because it
 * looks like the answer was given.
 */
export function gateSatisfied(dialog: DialogState): boolean {
  return (
    dialog.gate !== null && dialog.draft.confirmName === dialog.gate.expected
  );
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
  const hosted = visibleChoices(dialog).findIndex((choice) => choice.selected);
  return hosted === -1 ? 0 : hosted;
}

/** The rows the host's filter rule makes visible for the browser's local draft. */
export function visibleChoices(dialog: DialogState): readonly DialogChoice[] {
  if (!dialog.listFilter || dialog.draft.text === "") {
    return dialog.list;
  }
  const needle = dialog.draft.text.toLowerCase();
  return dialog.list.filter((choice) =>
    choice.label.toLowerCase().includes(needle),
  );
}

/**
 * 1e's right-hand state. `run from base` hides the branch field, because there
 * is nothing to name — so a confirm that toggles it must not also send text the
 * host would ignore.
 */
export function branchFieldVisible(dialog: DialogState): boolean {
  return dialog.input !== null;
}

/** The host's base target has no branch field; the other new-agent targets do. */
export function runsOnBase(dialog: DialogState): boolean {
  return dialog.kind === "new_agent" && dialog.input === null;
}

/** The button that `Enter` presses: the first one, which is the affirmative
 * action in every dialog the host builds. `null` when there is none. */
export function primaryKey(dialog: DialogState): DialogKey | null {
  return dialog.buttons[0] ?? null;
}

/** Whether the dialog offers a `Tab` option at all (only 1e's form does). */
export function hasToggle(dialog: DialogState): boolean {
  return toggleButton(dialog) !== null;
}

/**
 * The host-authored title for the current dialog target (§6.5 R19).
 */
export function dialogTitle(dialog: DialogState): string {
  return dialog.title;
}

/** The host's target-cycling button, if this dialog has one. */
export function toggleButton(dialog: DialogState): DialogKey | null {
  return dialog.buttons.find((button) => button.key === "Tab") ?? null;
}

/** The button the host marked as this dialog's cancel (`n`, `c`, `Esc`), or
 * `null` when it showed none. */
export function cancelButton(dialog: DialogState): DialogKey | null {
  return dialog.buttons.find((button) => button.cancels) ?? null;
}

/**
 * The keyed buttons a click or a keypress may *decide* with, in display order —
 * everything except the toggle, which flips rather than decides, and the cancel,
 * which is `dialog_cancel`'s job rather than a confirm's.
 *
 * Dropping the cancel here is what leaves 1g's step 1 with the two buttons the
 * artboard draws instead of three, two of which cancelled (§6.5 R19). `Esc` is
 * still excluded by name as well: the host refuses a confirm carrying it, so a
 * host that named no cancel at all must not turn its `Esc` button into one.
 */
export function decidingKeys(dialog: DialogState): readonly DialogKey[] {
  const toggleKey = toggleButton(dialog)?.key;
  return dialog.buttons.filter(
    (button) =>
      !button.cancels &&
      button.key !== "Esc" &&
      button.key !== toggleKey,
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
  /** 1g's second step rides on the deciding frame, which is what makes "the
   * seat that typed the name is the seat that confirms" structural: there is no
   * armed state on the host for a takeover to inherit. Sent only for the button
   * the host said it guards — spraying it at every confirm would teach the host
   * to expect it where it means nothing. */
  if (choice !== null && gatedKey(dialog, choice)) {
    args["confirm_name"] = dialog.draft.confirmName;
  }
  if (choice !== null && choice !== primaryKey(dialog)?.key) {
    args["choice"] = choice;
  }
  if (visibleChoices(dialog).length > 0) {
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
