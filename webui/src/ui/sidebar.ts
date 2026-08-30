import { findProject } from "../state/model";
import {
  sessionStatusText,
  statusGlyph,
  statusTone,
  statusWord,
} from "../state/status";
import type { Session } from "../state/model";
import type { AppState } from "../state/types";
import { clear, el, spinnerGlyph } from "./dom";
import type { Child, Region } from "./dom";
import { toneClass } from "./tone";
import type { Store } from "./store";

/**
 * Region 3 of 7 — the agents sidebar, with 1a's three-line session block.
 *
 * The three lines are: name (with caret + status glyph), agent + status, and
 * the facts line. Which facts appear on line three is the part that carries
 * 2g's dim-tier rule:
 *
 *   - `no-upstream` and `git: ?` are **facts** — one says a push would have
 *     nowhere to go, the other says git has not answered yet. Both render at
 *     `--fd-text-quiet` (4.8:1). 1a's markup drew them at the decoration tier,
 *     but 2g and spec §5.1 supersede it by name, and the rule decides it
 *     anyway: delete either string and you have lost a fact.
 *   - `·set` marks a status a human set by hand. Also a fact, and 2g files
 *     "manual override" under `--fd-accent`, so that is where it goes.
 *   - The only decoration in this region is the session *count* in the footer,
 *     which 2g lists explicitly ("item counts").
 *
 * **D3 again.** The whole block is one `<button>` — real, focusable, keyboard-
 * operable — whose tooltip says the selection moves the desktop too. The `✕` is
 * a sibling rather than a nested button (nested interactives are invalid and
 * unreachable by keyboard), positioned over the row's first line.
 *
 * ## Below 900px it is the same component, slid over (1h, §6.5 R17)
 *
 * 1h: *"Below 900px the sidebar becomes a slide-over invoked from a session
 * chip in the project row."* Turn 3 never drew it, so the mechanism is 2e's,
 * reused rather than re-invented: the element is toggled with the `hidden`
 * property, it is absolutely positioned over the body, and it keeps 2e's
 * posture — **no `aria-modal`, no focus trap, no scrim that swallows clicks**,
 * and the terminal behind it stays live. The one difference is the edge: 2e
 * arrives from the right, this arrives from the **left**, because that is
 * where 1a's column is and two panels sharing an edge would fight over it.
 *
 * Nothing below branches on the width except the `hidden` toggle and the close
 * button, which is the point: at narrow this is not a different sidebar, it is
 * the same sidebar somewhere else.
 */
export function createSidebar(store: Store): Region {
  const list = el("ul", { class: "fd-sessions" });
  const footer = el("footer", { class: "fd-sidebar__foot" });

  /**
   * 2e's `a close`, in the sidebar's own key. Always in the DOM and hidden by
   * CSS at wide rather than rendered conditionally: a column that is always on
   * screen has nothing to close, and a `display: none` button is not focusable,
   * so a wide keyboard user never meets it.
   */
  const close = el(
    "button",
    {
      class: "fd-sidebar__close",
      title: "close the agents list",
      attrs: { type: "button" },
    },
    [el("span", { class: "fd-key", text: "s" }), " close"],
  );
  close.addEventListener("click", () => {
    store.dispatch({ type: "sidebar/set", open: false });
  });

  const aside = el(
    "aside",
    { class: "fd-sidebar", attrs: { "aria-label": "Agents" } },
    [
      /**
       * No `.fd-spacer` here on purpose: 1a centres this title, and a flexible
       * gap would fill the row and push "Agents" to the left at *both* widths.
       * The close button takes `margin-left: auto` in the narrow stylesheet
       * instead, so the wide title is untouched.
       */
      el("div", { class: "fd-sidebar__title" }, [
        el("span", { class: "fd-sidebar__heading", text: "Agents" }),
        close,
      ]),
      list,
      footer,
    ],
  );

  function render(state: AppState): void {
    clear(list);
    clear(footer);

    /**
     * The whole of the narrow layout's effect on this component. At wide the
     * sidebar is 1a's column and is never hidden; at narrow it is on screen
     * only while the reader asked for it.
     */
    aside.hidden = state.width === "narrow" && !state.sidebarOpen;

    const selection = state.selection;
    const project =
      selection === null ? null : findProject(state.projects, selection.projectId);
    const sessions = project?.sessions ?? [];

    for (const session of sessions) {
      list.append(
        sessionRow(session, session.id === selection?.sessionId, store),
      );
    }

    /**
     * 1a's footer is a count plus the way to make another agent; 1b's is the
     * movement keys, because in App mode the keyboard is pointed here.
     */
    if (state.mode === "app") {
      footer.append(
        el("span", { class: "fd-key", text: "↑↓" }),
        " move · ",
        el("span", { class: "fd-key", text: "Enter" }),
        " focus terminal",
      );
    } else {
      footer.append(
        `${sessions.length} session${sessions.length === 1 ? "" : "s"} · `,
        el("span", { class: "fd-key", text: "Ctrl-g" }),
        " → “new agent”",
      );
    }
  }

  return { el: aside, update: render };
}

function sessionRow(
  session: Session,
  selected: boolean,
  store: Store,
): HTMLElement {
  const tone = toneClass(statusTone(session.status));
  const glyph = statusGlyph(session.status);

  const head = el("span", { class: "fd-session__head" }, [
    el("span", {
      class: "fd-session__caret",
      text: "▸",
      attrs: { "aria-hidden": "true" },
    }),
    glyph === "spinner"
      ? spinnerGlyph(tone)
      : el("span", {
          class: `fd-glyph ${tone}`,
          /** §5.1: `○` is the glyph for "nobody is claiming to know". */
          text: glyph === "hollow" ? "○" : "●",
          attrs: { "aria-hidden": "true" },
        }),
    el("span", { class: "fd-session__name", text: session.name }),
  ]);

  const select = el(
    "button",
    {
      class: "fd-session__select",
      title: "select this session — this also moves the desktop's selection",
      attrs: {
        type: "button",
        ...(selected ? { "aria-current": "true" } : {}),
      },
    },
    [head, statusLine(session, tone), factsLine(session)],
  );
  select.addEventListener("click", () => {
    store.dispatch({ type: "selection/session", sessionId: session.id });
    /**
     * Picking a session is the errand the slide-over was opened for, so it
     * closes behind you — 2e's own rule (`jumpTo` closes the feed on a jump)
     * applied to the same shape. A no-op at wide, where the reducer refuses a
     * `sidebar/set` outright, so this costs the column nothing.
     */
    store.dispatch({ type: "sidebar/set", open: false });
  });

  const close = el("button", {
    class: "fd-session__close",
    text: "✕",
    title: "closing a session is a destructive operation — M2 (D8)",
    attrs: {
      type: "button",
      disabled: "",
      "aria-label": `Close session ${session.name}`,
    },
  });

  return el(
    "li",
    { class: "fd-session", attrs: { "data-selected": String(selected) } },
    [select, close],
  );
}

function statusLine(session: Session, tone: string): HTMLElement {
  /** A session with no agent process yet gets 1a's italic prose instead of a
   * status chip, because there is no status to report. */
  if (session.startingNote !== null) {
    return el("span", {
      class: "fd-session__starting",
      text: sessionStatusText(session),
    });
  }

  const parts: Child[] = [
    el("span", { text: session.agent }),
    " ",
    el("span", { class: tone, text: sessionStatusText(session) }),
  ];
  if (session.manual) {
    parts.push(
      el("span", {
        class: "fd-tone-accent",
        text: " ·set",
        title: "this status was set by hand, not observed",
      }),
    );
  }
  return el("span", { class: "fd-session__agent" }, parts);
}

function factsLine(session: Session): HTMLElement {
  const parts: Child[] = [];

  /**
   * A manual override replaces the git facts with the truth underneath it
   * (1a: `really: idle`). Showing both would put two competing statuses on one
   * row; the observed one is the one you could act on wrongly.
   */
  if (session.manual && session.observed !== null) {
    parts.push(
      el("span", { class: "fd-tone-quiet", text: "really:" }),
      el("span", {
        class: toneClass(statusTone(session.observed)),
        text: statusWord(session.observed),
      }),
    );
  } else if (session.git.kind === "no_upstream") {
    parts.push(
      el("span", {
        class: "fd-tone-quiet",
        text: "no-upstream",
        title: "this branch has no upstream — a push would have nowhere to go",
      }),
    );
  } else if (session.git.kind === "unknown") {
    parts.push(
      el("span", {
        class: "fd-tone-quiet",
        text: "git: ?",
        title: "git has not answered for this worktree yet",
      }),
    );
  } else {
    const git = session.git;
    if (git.recovered) {
      parts.push(el("span", { class: "fd-tone-elsewhere", text: "[recovered]" }));
    }
    if (git.dirty) {
      parts.push(el("span", { class: "fd-tone-focus", text: "~dirty" }));
    }
    if (git.added > 0 || git.removed > 0) {
      parts.push(
        el("span", {
          class: "fd-tone-accent",
          text: `+${git.added} -${git.removed}`,
        }),
      );
    }
    if (git.drift !== null) {
      parts.push(
        el("span", {
          class: "fd-tone-elsewhere",
          text: `drift:${git.drift}`,
          title: "commits this worktree has drifted from its base",
        }),
      );
    }
  }

  return el("span", { class: "fd-session__facts" }, parts);
}
