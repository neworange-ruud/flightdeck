import { JUMP_HINT, newestFirst, transitionText } from "../state/activity";
import { statusGlyph, statusTone } from "../state/status";
import type { ActivityEvent } from "../state/model";
import type { AppState } from "../state/types";
import { clear, el, spinnerGlyph } from "./dom";
import type { Region } from "./dom";
import { toneClass } from "./tone";

/**
 * Artboard `2e — ACTIVITY FEED`: a right-edge slide-over.
 *
 * ## Why it is not a modal, and why that is load-bearing
 *
 * D11 makes this feed **the entire substitute for OS notifications** — Web Push
 * is structurally blocked under D1, so there is no second channel. A
 * notification surface that blocked the screen would be worse than no
 * notifications at all: it would interrupt the terminal a user is reading in
 * order to tell them about a terminal they are not.
 *
 * So: `role="complementary"`, **no `aria-modal`**, no focus trap, no scrim that
 * swallows clicks, and the terminal behind it stays live and clickable. 2e says
 * it in three words — *never a modal* — and the tests assert the absence of
 * `aria-modal` rather than the presence of a class, because it is the semantics
 * that matter.
 *
 * ## It opens on history, not silence
 *
 * The host retains `min(200 events, 24h)` and sends it on attach
 * (`Snapshot::activity`), so a tab opened five minutes after an agent finished
 * still shows that it finished. The feed renders whatever the store holds; it
 * never starts empty because *this tab* was not watching.
 *
 * ## Rows move the desktop
 *
 * D3: selecting a session here selects it on the desktop too. 2e's judgement is
 * that a hover title is the only warning that needs — `jump · also moves the
 * desktop` — because selection is reversible and cheap. The row is a real
 * `<button>` so the title is reachable by keyboard focus as well as hover.
 */

export interface ActivityFeedOptions {
  /** D3: jump to a session, in whichever project it lives in. */
  readonly onJump?: (event: ActivityEvent) => void;
  /** 2e's `a close`. */
  readonly onClose?: () => void;
}

export function createActivityFeed(options: ActivityFeedOptions = {}): Region {
  const list = el("div", { class: "fd-feed__list" });
  const foot = el("div", { class: "fd-feed__foot" }, [
    "the host keeps the last ",
    el("span", { class: "fd-feed__retention", text: "200 events / 24h" }),
    ", so a fresh tab opens on history, not silence. The desktop still posts its own OS notifications — this is a second record, not the only one.",
  ]);
  const closeKey = el("span", { class: "fd-key", text: "a" });
  const close = el("button", { class: "fd-feed__close", attrs: { type: "button" } }, [
    closeKey,
    " close",
  ]);
  close.addEventListener("click", () => options.onClose?.());

  const panel = el(
    "aside",
    {
      class: "fd-feed",
      attrs: {
        /** A complementary landmark, not a dialog: it augments the screen
         * rather than taking it over. See the module doc. */
        role: "complementary",
        "aria-label": "Activity",
      },
    },
    [
      el("div", { class: "fd-feed__head" }, [
        el("span", { class: "fd-feed__title", text: "Activity" }),
        /** D11: the feed is global across projects, which is the whole reason
         * it is useful — the session that needs you is usually in a project you
         * are not looking at. */
        el("span", { class: "fd-feed__scope", text: "all projects" }),
        el("div", { class: "fd-spacer" }),
        close,
      ]),
      list,
      foot,
    ],
  );

  function render(state: AppState): void {
    panel.hidden = !state.feedOpen;
    panel.setAttribute("data-open", String(state.feedOpen));
    if (!state.feedOpen) {
      return;
    }
    clear(list);
    const events = newestFirst(state.activity);
    if (events.length === 0) {
      list.append(emptyState(state));
      return;
    }
    for (const event of events) {
      list.append(row(event, options.onJump));
    }
  }

  return { el: panel, update: render };
}

/**
 * 2e's empty state, which says what *would* land here and what is true right
 * now. "Nothing has changed in 24 hours" is a much stronger statement than an
 * empty box, and it is the honest one: the host's retention window is where the
 * 24 hours comes from.
 */
function emptyState(state: AppState): HTMLElement {
  const idle = state.projects.reduce(
    (count, project) => count + project.sessions.length,
    0,
  );
  return el("div", { class: "fd-feed__empty" }, [
    el("p", {
      class: "fd-feed__empty-title",
      text: "Nothing has changed in 24 hours.",
    }),
    el("p", {
      class: "fd-feed__empty-body",
      text: `Status transitions land here — an agent finishing, stalling on a question, or failing. ${idle} session${
        idle === 1 ? " is" : "s are"
      } idle right now.`,
    }),
  ]);
}

function row(
  event: ActivityEvent,
  onJump: ((event: ActivityEvent) => void) | undefined,
): HTMLElement {
  const glyph = statusGlyph(event.to);
  const tone = toneClass(statusTone(event.to));

  const button = el(
    "button",
    {
      class: "fd-feed__row",
      /** D3, said out loud on every row. */
      title: JUMP_HINT,
      attrs: {
        type: "button",
        "data-tier": event.tier,
        "data-read": String(event.read),
      },
    },
    [
      el("span", { class: "fd-feed__line" }, [
        glyph === "spinner"
          ? spinnerGlyph(tone)
          : el("span", {
              class: `fd-glyph ${tone}`,
              text: glyph === "hollow" ? "○" : "●",
              attrs: { "aria-hidden": "true" },
            }),
        el("span", { class: "fd-feed__session", text: event.sessionName }),
        el("span", { class: "fd-feed__project", text: event.projectName }),
        el("div", { class: "fd-spacer" }),
        el("span", { class: "fd-feed__when", text: event.atLabel }),
      ]),
      /**
       * The transition, with `unknown → unknown · Codex CLI reports no
       * lifecycle` rendered as **data**: both ends of the arrow come from the
       * host and the reason is its verbatim string. See `transitionText` for
       * the two things this must never do.
       */
      el("span", { class: "fd-feed__transition", text: transitionText(event) }),
      /** The hover copy, also present as text for touch and screen readers —
       * a `title` alone is invisible on a phone, which is the device 2e's
       * slide-over shape exists for. */
      el("span", { class: "fd-feed__jump", text: JUMP_HINT }),
    ],
  );
  button.addEventListener("click", () => onJump?.(event));
  return button;
}
