import { connectionStrip, hasControl } from "../state/connection";
import type { ConnectionStrip, StripAction } from "../state/connection";
import { unreadChip } from "../state/activity";
import { viewerChipText, viewerChipTitle } from "../state/seats";
import type { StatusGlyph } from "../state/model";
import type { AppState } from "../state/types";
import { clear, el, separator, spinnerGlyph } from "./dom";
import type { Child, Region } from "./dom";

/**
 * Region 7 of 7 — the status bar (1a/1b/1c bottom strip), now carrying every
 * state of artboard `2c — CONNECTION STATES`.
 *
 * The three rules 2c states, and where each one lives:
 *
 *   1. **The position never moves**, so a glance always lands on it. The
 *      structure below guarantees it mechanically: `.fd-spacer` is always the
 *      element immediately before `.fd-conn`, in every state, so the
 *      connection group is always pushed to the same edge. That is asserted
 *      directly in `src/ui/connectionStates.test.ts` rather than eyeballed.
 *   2. **Anything that costs the user control drains the mode chip** — see
 *      `modeChip`, which asks `hasControl` rather than re-deciding.
 *   3. **The whole bar takes the state's frame colour**, the only chrome in the
 *      app that ever changes hue: `data-frame` on the bar, resolved to a token
 *      in `src/style/states.css`.
 *
 * `18ms` and the viewer chip are both lifted off `--fd-text-decor`, where 1a
 * drew them, onto `--fd-text-quiet`. Both are facts about whether what you are
 * looking at can be trusted: the round-trip time is the difference between
 * "live" and "live-ish", and the seat list is how you know the other seat is
 * your own desktop and not a second person. 2g's rule decides it — delete
 * either and a fact is gone.
 */

export type ModeChipTone = "terminal" | "app" | "drained" | "stopped";

/**
 * §5.1: **losing control drains the mode chip.** Any state that costs the user
 * control renders `MODE: —`, because naming a mode while keystrokes are not
 * arriving is a lie. Pure, and exported, so the rule is a unit test rather
 * than a screenshot.
 *
 * Two states are worth reading twice:
 *
 *   - **`stopped` replaces the chip rather than draining it** (2c:
 *     `FLIGHTDECK STOPPED`). "No mode" understates a host that has exited —
 *     there is nothing left to have a mode *in* — and the whole point of Q5 is
 *     to stop describing a dead host in the vocabulary of a live one.
 *   - **A read-only seat drains it too.** D14 makes observation a real mode,
 *     but an observer's `Input` frames are answered `AckOutcome::Ignored`, so a
 *     chip claiming `MODE: TERMINAL` would be exactly the lie this rule exists
 *     to prevent. The strip says `read-only · your keystrokes are not being
 *     sent` next to it, so the drained chip is explained rather than mysterious.
 *
 * Version mismatch is deliberately *not* here: 2c keeps the mode chip intact,
 * because a stale tab has lost nothing but its version. That position is also
 * why the reload chip's `Enter` is not bound globally — see `actionButton`
 * below and `specs/WEB_INTERFACE.md` §6.5 R9.
 */
export function modeChip(state: AppState): {
  readonly text: string;
  readonly tone: ModeChipTone;
} {
  if (state.connection === "stopped") {
    return { text: "FLIGHTDECK STOPPED", tone: "stopped" };
  }
  if (!hasControl(state)) {
    return { text: "MODE: —", tone: "drained" };
  }
  return state.mode === "terminal"
    ? { text: "MODE: TERMINAL", tone: "terminal" }
    : { text: "MODE: APP", tone: "app" };
}

export interface StatusBarOptions {
  /** 2c's keyed buttons: `r Retry now`, `Enter Reload for v1.17.0`, `Enter
   * Enter a code`. The bar reports the intent; who acts on it is `app.ts`. */
  readonly onAction?: (action: StripAction) => void;
  /** The unread chip is the pointer half of `a` (2e). */
  readonly onOpenFeed?: () => void;
}

export function createStatusBar(options: StatusBarOptions = {}): Region {
  const bar = el("div", {
    class: "fd-statusbar",
    attrs: { role: "status", "aria-label": "Status", "data-frame": "neutral" },
  });

  function render(state: AppState): void {
    clear(bar);
    const chip = modeChip(state);
    const strip = connectionStrip(state);
    bar.setAttribute("data-frame", strip.frame);

    const parts: Child[] = [
      el("span", {
        class: "fd-mode",
        text: chip.text,
        attrs: { "data-tone": chip.tone },
      }),
    ];

    /**
     * 2c replaces the key hints with a sentence in every state that has
     * something to say about the user's keystrokes, and keeps them in the two
     * states that do not (`connected`, `connecting`).
     */
    if (strip.message === null) {
      for (const hint of hintsFor(state)) {
        parts.push(separator(), hint);
      }
    } else {
      parts.push(
        separator(),
        el("span", { class: "fd-statusbar__message", text: strip.message }),
      );
    }

    parts.push(
      /** Rule 1. This spacer is load-bearing: it is what keeps `.fd-conn` in
       * the same place in all nine states, so do not add a second one after
       * it and do not give the connection group a margin of its own. */
      el("div", { class: "fd-spacer" }),
      connectionGroup(strip),
    );

    /**
     * Right of the connection group, in a fixed order so the strip's own
     * position is never disturbed: the seat chip (a fact about trust), then
     * the state's one action, then the unread chip, then the host-update chip.
     */
    if (strip.action === null && strip.note === null) {
      parts.push(separator(), viewerChip(state));
    }
    if (strip.note !== null) {
      parts.push(
        separator(),
        el("span", { class: "fd-statusbar__note", text: strip.note }),
      );
    }
    if (strip.staleChip !== null) {
      parts.push(
        el("span", {
          class: "fd-stalechip",
          text: strip.staleChip,
          title:
            "the terminal below is a photograph from this long ago — nothing you type is arriving, but it is being kept",
        }),
      );
    }
    if (strip.action !== null) {
      parts.push(actionButton(strip.action, options.onAction));
    }
    parts.push(unreadChipEl(state, options.onOpenFeed));
    parts.push(
      state.update === null
        ? el("span", { class: "fd-statusbar__pad" })
        : el("span", { class: "fd-update" }, [
            el("span", { text: "●", attrs: { "aria-hidden": "true" } }),
            `${state.update.version} available`,
          ]),
    );

    for (const part of parts) {
      if (part !== null && part !== undefined && part !== false) {
        bar.append(part);
      }
    }
  }

  return { el: bar, update: render };
}

/** The `● connected 18ms` group, in the one shape every 2c row uses. */
function connectionGroup(strip: ConnectionStrip): HTMLElement {
  return el("span", { class: "fd-conn" }, [
    glyph(strip.status.glyph, strip.status.tone),
    strip.status.text,
    strip.status.detail === null
      ? null
      : el("span", { class: "fd-conn__latency", text: strip.status.detail }),
  ]);
}

function glyph(kind: StatusGlyph, tone: string): HTMLElement {
  if (kind === "spinner") {
    return spinnerGlyph(tone);
  }
  return el("span", {
    class: `fd-glyph ${tone}`,
    /** Hollow means nobody is claiming anything is running (§5.1's `○`). */
    text: kind === "hollow" ? "○" : "●",
    attrs: { "aria-hidden": "true" },
  });
}

/**
 * 2f's viewer chip: `desktop + this tab ✎`.
 *
 * Named seats, not a counter that implies a crowd — and the naming is not
 * decoration: the reason a second seat is not alarming is that you can see it
 * is your own desktop.
 *
 * The `✎` is the second fact D14's revision made necessary, and it is the whole
 * reason `desktop + this tab` was not good enough any more. Several surfaces can
 * type; one of them is typing *now*, and the others are being refused. That is
 * the only honest answer to "why did my keystroke not appear", so the chip says
 * it rather than leaving the user to guess — and the desktop's own status bar
 * carries the same fact, from the same seat rows.
 */
function viewerChip(state: AppState): HTMLElement {
  return el("span", {
    class: "fd-viewers",
    text: viewerChipText(state.seats, state.viewers),
    title: viewerChipTitle(state.seats),
  });
}

/**
 * `r Retry now` and `Enter Enter a code` are bound globally in `app.ts`,
 * because `disconnected` and `revoked` deliver input nowhere and the key is
 * therefore free. `reload`'s `Enter` is the one exception: 2c draws a version
 * mismatch as `● connected 21ms` with the mode chip intact (see
 * `ConnectionStatus`'s doc comment in `state/types.ts`), so `Enter` still
 * means "newline" or "focus the terminal" everywhere else and cannot be
 * claimed globally without contradicting that. Ruling recorded in
 * `specs/WEB_INTERFACE.md` §6.5 R9.
 */
function actionButton(
  action: StripAction,
  onAction: ((action: StripAction) => void) | undefined,
): HTMLElement {
  const scopedToFocus = action.kind === "reload";
  const button = el(
    "button",
    {
      class: "fd-stripaction",
      attrs: { type: "button", "data-tone": action.tone, "data-kind": action.kind },
      /** The visible text stays exactly `Enter` + the label (2c's own words);
       * the title carries the one fact the artboard has no room to print.
       * Omitted rather than set to `undefined` — `exactOptionalPropertyTypes`
       * treats those as different things, and `el` only checks presence. */
      ...(scopedToFocus
        ? { title: "Enter reloads only while this button is focused" }
        : {}),
    },
    [el("span", { class: "fd-stripaction__key", text: action.key }), action.label],
  );
  button.addEventListener("click", () => onAction?.(action));
  /**
   * A real `<button>` already activates on Enter/Space once focused, in every
   * browser — this handler is what makes that a fact a test can check rather
   * than an assumption about the platform (jsdom does not run the browser's
   * own keydown-activates-button default action). It is attached to the
   * button itself, so nothing fires unless the button is the event's target:
   * an `Enter` that bubbles up from anywhere else in the frame never reaches
   * it, which is the whole of what "focus-scoped" means in code.
   */
  button.addEventListener("keydown", (event: KeyboardEvent) => {
    if (event.key === "Enter") {
      event.preventDefault();
      onAction?.(action);
    }
  });
  return button;
}

/**
 * 2e's unread chip, in its four renderings.
 *
 * It is a `<button>` because it is the pointer-driven way into the feed — 2e
 * offers `a` **or the unread chip**, and a chip that only looked clickable
 * would leave a touch user with no way in at all (there is no `a` key on a
 * phone's lock screen).
 */
function unreadChipEl(
  state: AppState,
  onOpenFeed: (() => void) | undefined,
): HTMLElement {
  const chip = unreadChip(state.activity);
  const button = el(
    "button",
    {
      class: "fd-unread",
      title: chip.title,
      attrs: {
        type: "button",
        "data-tier": chip.tone,
        "aria-expanded": String(state.feedOpen),
        "aria-label": `Activity — ${chip.title}`,
      },
    },
    [
      chip.text,
      chip.key === null
        ? null
        : el("span", { class: "fd-key", text: chip.key }),
    ],
  );
  button.addEventListener("click", () => onOpenFeed?.());
  return button;
}

/**
 * The hint row, straight from the artboards: 1a in Terminal mode, 1b in App
 * mode, 1c in split. `Ctrl-g` is the only chord the app claims (§5), so it is
 * the only one that appears in every variant.
 */
function hintsFor(state: AppState): readonly HTMLElement[] {
  if (state.layout === "split") {
    return [
      hint("SPLIT", "3 terminals"),
      hint("←/→", "move focus"),
      hint("Ctrl-g", "palette → “split”"),
    ];
  }
  if (state.mode === "app") {
    return [
      hint("Enter", "focus terminal"),
      /** 2e puts `a activity` in App mode's hint row, next to `↑↓ sessions`. */
      hint("a", "activity"),
      hint("Ctrl-g", "command palette"),
      hint("?", "help"),
    ];
  }
  return [
    hint("Esc Esc", "app commands"),
    hint("Ctrl-g", "command palette"),
    hint("click outside", "release keys"),
  ];
}

function hint(key: string, label: string): HTMLElement {
  return el("span", { class: "fd-hint" }, [
    el("span", { class: "fd-key", text: key }),
    ` ${label}`,
  ]);
}
