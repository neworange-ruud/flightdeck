import {
  accessCopy,
  accessFooter,
  attemptsLine,
  codeBoxes,
} from "../state/access";
import type { AppState } from "../state/types";
import { clear, el } from "./dom";
import type { Child, Region } from "./dom";

/**
 * Artboard `2b — BROWSER-SIDE ACCESS SCREENS`: code entry, rejected, revoked,
 * and the rate-limited fourth the host models.
 *
 * ## Why this is an overlay inside the frame, not a separate screen
 *
 * The seam note called for "a screen chooser above `createApp`". Read 2b's
 * three panels again and they are all drawn *inside the app frame*: the logo
 * band is above them, a footer strip is below them, and the revoked panel has a
 * running agent's output visible behind it. 2b even says so in words —
 * *"everything you can see below this dialog is a photograph from the moment
 * access ended"*. A separate screen could not show that, and would have to
 * reproduce the logo band and the footer to look right.
 *
 * So this is a layer inside the frame, switched by one attribute
 * (`data-access` on `.fd-frame`), which also hides the git bar and status bar —
 * there is no session to describe, and 2b replaces both with its own footer.
 * The deviation is deliberate and reported.
 *
 * ## Q7's posture, applied
 *
 * *Never claim a protection we cannot deliver.* Nothing here says "secure" or
 * "private". The revoked screen states that the agents kept running and that
 * the picture behind is a photograph, because that is what is true. The
 * rate-limited screen offers **no** primary button, because the host would
 * refuse it and a button that cannot work is a claim we cannot keep.
 */

export interface AccessScreenOptions {
  /** `Enter` with four digits: exchange them (`POST /auth/exchange`). */
  readonly onSubmit?: () => void;
  /** A digit was pressed on the on-screen keypad. */
  readonly onDigit?: (digit: string) => void;
  /** 2b's `Enter a new code` — go back to a blank keypad. */
  readonly onRetry?: () => void;
  /** 2b's `Esc Stay here` — leave the photograph up and get out of the way. */
  readonly onDismiss?: () => void;
}

export function createAccessScreen(options: AccessScreenOptions = {}): Region {
  const panel = el("div", { class: "fd-access__panel" });
  const footer = el("div", { class: "fd-access__foot" });
  const layer = el(
    "div",
    {
      class: "fd-access",
      attrs: {
        /**
         * `role="dialog"` with no `aria-modal`: it *is* a dialog — it demands a
         * decision — but it deliberately does not claim the rest of the page is
         * unavailable, because 2b's whole point on the revoked screen is that
         * what is behind it is still worth reading.
         */
        role: "dialog",
        "aria-label": "Access",
      },
    },
    [panel, footer],
  );

  function render(state: AppState): void {
    const access = state.access;
    layer.hidden = access === null;
    if (access === null) {
      return;
    }
    const copy = accessCopy(access);
    layer.setAttribute("data-screen", access.screen);
    layer.setAttribute("data-tone", copy.tone);

    clear(panel);
    const head = el("div", { class: "fd-access__head" }, [
      el("span", { class: "fd-access__title", text: copy.title }),
      copy.eyebrow === null
        ? null
        : el("span", { class: "fd-access__eyebrow", text: copy.eyebrow }),
    ]);

    const body: Child[] = [
      copy.acceptsCode ? codeRow(access.code, options.onDigit) : null,
      /** 2b's rejected screen shows the digits that failed, in the alert
       * frame — which is what makes "it was mistyped" checkable. */
      !copy.acceptsCode || access.refused === ""
        ? null
        : el("p", {
            class: "fd-access__refused",
            text: `${access.refused} was refused`,
          }),
      el("p", { class: "fd-access__body", text: copy.body }),
      copy.detail === null
        ? null
        : el("p", { class: "fd-access__detail", text: copy.detail }),
      copy.steps.length === 0 ? null : stepList(copy.steps),
      actionRow(copy, options),
    ];

    panel.append(head);
    for (const child of body) {
      if (child !== null && child !== undefined && child !== false) {
        panel.append(child);
      }
    }

    clear(footer);
    const attempts = attemptsLine(access);
    footer.append(
      el("span", {
        class: "fd-access__host",
        text: accessFooter(state.host),
      }),
    );
    if (attempts !== null) {
      /**
       * 2b's exact footer sentence, e.g. `3 attempts left before this address
       * is rate-limited for 60s`. The number is the host's — the browser never
       * counts attempts, because it would disagree with the limiter that
       * actually decides.
       */
      footer.append(
        el("span", { class: "fd-access__attempts", text: attempts }),
      );
    }
  }

  return { el: layer, update: render };
}

/** 2b's four boxes, with the caret on the first empty one. */
function codeRow(
  code: string,
  onDigit: ((digit: string) => void) | undefined,
): HTMLElement {
  const row = el("div", {
    class: "fd-code",
    attrs: { role: "group", "aria-label": "Four-digit code" },
  });
  for (const box of codeBoxes(code)) {
    row.append(
      el(
        "span",
        {
          class: "fd-code__box",
          attrs: {
            "data-filled": String(box.digit !== null),
            "data-caret": String(box.caret),
          },
        },
        [
          box.digit ?? "",
          /** The caret is a real element with real text, not a CSS
           * pseudo-element, so a test and a screen reader can both see it. */
          box.caret
            ? el("span", {
                class: "fd-code__caret",
                text: " ",
                attrs: { "aria-hidden": "true" },
              })
            : null,
        ],
      ),
    );
  }
  /**
   * A keypad, because a phone is the device this screen exists for (2a's QR
   * points a phone here) and a phone has no physical keyboard to catch
   * `keydown` from until something is focused.
   */
  const pad = el("div", {
    class: "fd-code__pad",
    attrs: { role: "group", "aria-label": "Keypad" },
  });
  for (const digit of "0123456789") {
    const key = el("button", {
      class: "fd-code__key",
      text: digit,
      attrs: { type: "button" },
    });
    key.addEventListener("click", () => onDigit?.(digit));
    pad.append(key);
  }
  return el("div", { class: "fd-code__wrap" }, [row, pad]);
}

function stepList(steps: readonly string[]): HTMLElement {
  const list = el("ol", { class: "fd-access__steps" });
  for (const step of steps) {
    list.append(el("li", { text: step }));
  }
  return list;
}

function actionRow(
  copy: ReturnType<typeof accessCopy>,
  options: AccessScreenOptions,
): HTMLElement {
  const row = el("div", { class: "fd-access__actions" });
  if (copy.primary !== null) {
    const primary = keyedButton(
      "fd-access__primary",
      copy.primary.key,
      copy.primary.label,
    );
    primary.addEventListener("click", () => {
      /** The same button does both jobs across the four screens: on a screen
       * that takes a code it submits, on one that does not it starts over. */
      if (copy.acceptsCode) {
        options.onSubmit?.();
      } else {
        options.onRetry?.();
      }
    });
    row.append(primary);
  }
  if (copy.secondary !== null) {
    const secondary = keyedButton(
      "fd-access__secondary",
      copy.secondary.key,
      copy.secondary.label,
    );
    secondary.addEventListener("click", () => options.onDismiss?.());
    row.append(secondary);
  }
  return row;
}

function keyedButton(
  className: string,
  key: string,
  label: string,
): HTMLButtonElement {
  return el("button", { class: className, attrs: { type: "button" } }, [
    el("span", { class: "fd-key-box", text: key }),
    label,
  ]);
}
