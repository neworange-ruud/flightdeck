import {
  BROWSER_KEYS,
  BROWSER_SECTION_TITLE,
  HOST_HELP_ABSENT,
  HOST_SECTION_NOTE,
  HOST_SECTION_TITLE,
} from "../state/help";
import type { GitStatusPanel, HelpRow } from "../state/model";
import type { AppState } from "../state/types";
import { append, clear, el, hint, hostOnlyBadge } from "./dom";
import type { Child, Region } from "./dom";

/**
 * The three read-only panels — help, About, and SPECS §21's git status
 * (`remote-control-ll5.8`, `specs/WEB_INTERFACE.md` §6.5 R16).
 *
 * ## They are one component because they are one kind of thing
 *
 * Each is a titled panel that **states facts and asks nothing**. R8 says that
 * about git status in as many words — *"it is not one of D13's dialogs;
 * nothing is being asked, so there is nothing to confirm or cancel"* — and it
 * is equally true of the other two. So they share a shell, a frame colour, a
 * keyboard (`Esc`) and this file, and `AppState.readOnly` holds one of them at
 * a time so "two panels at once" is not a state that exists.
 *
 * ## Modal-shaped, not a slide-over — and why that is not 2e's rule broken
 *
 * §5.1 makes the activity feed a right-edge slide-over and *"never a modal"*,
 * for a reason that is specific to it: D11 makes the feed the entire substitute
 * for OS notifications, and a notification surface that blocked the screen
 * would interrupt the terminal you are reading in order to tell you about one
 * you are not. **None of these three is a notification.** Every one of them is
 * on screen because the reader just asked for it, by name, from the palette —
 * which is exactly the posture 1d's palette and 1f's configuration manager
 * already take, both of them centred panels over a dimmed frame. So these
 * follow 1d, and the feed keeps following 2e.
 *
 * Like those two, and unlike a `<dialog>`, they carry `role="dialog"` with no
 * `aria-modal` and no focus trap: the frame behind stays readable, which is
 * the point of a panel you opened to *compare* something against what is
 * behind it.
 *
 * ## The frame is blue, and the blue is the artboards' own
 *
 * 1g's caption is the legend for the whole dialog family: *"Cyan frame =
 * confirm/select, blue = notification, red = destructive."* Cyan is spoken for
 * (1d's palette, 1e's form, 1f's manager — all of them ask you to choose), red
 * is spoken for (1g), magenta is 2f's. Blue — 2g's `--fd-info` — is the one
 * frame colour the legend named and nothing has claimed, and *notification* is
 * precisely what these are. Sharing it across all three makes the panel's own
 * colour say what R8 says in prose: **there is nothing here to answer.**
 *
 * ## What none of them may do
 *
 * Render a fact the host did not send. The help panel's host half comes from
 * `Snapshot::help` and is empty — with a sentence saying so — when the host
 * sent none; About comes from `Snapshot::about`; and every row of the git
 * panel comes from the `ServerMsg::GitStatus` frame that opened it, with the
 * two absent-able facts rendered as *absent* rather than as zero. See
 * `state/help.ts` for the one list this file is allowed to author, and why.
 */

export interface InfoOverlayOptions {
  /** `Esc`, the close button, or a click outside the panel. */
  readonly onClose?: () => void;
}

export function createInfoOverlay(options: InfoOverlayOptions = {}): Region {
  const titleEl = el("span", { class: "fd-info__title" });
  const subtitleEl = el("span", { class: "fd-info__subtitle" });
  const bodyEl = el("div", { class: "fd-info__body" });
  const footEl = el("div", { class: "fd-info__foot" }, [hint("Esc", "close")]);

  const closeEl = el(
    "button",
    { class: "fd-info__close", attrs: { type: "button" } },
    [el("span", { class: "fd-key", text: "Esc" }), " close"],
  );
  closeEl.addEventListener("click", () => options.onClose?.());

  const panel = el("div", { class: "fd-info__panel" }, [
    el("div", { class: "fd-info__head" }, [titleEl, subtitleEl, closeEl]),
    bodyEl,
    footEl,
  ]);

  const layer = el(
    "div",
    {
      class: "fd-info",
      /** A panel, not a question: `role="dialog"` for the landmark and the
       * label, deliberately without `aria-modal` — see the module doc. */
      attrs: { role: "dialog", "aria-label": "FlightDeck information" },
    },
    [panel],
  );

  function render(state: AppState): void {
    const overlay = state.readOnly;
    layer.hidden = overlay === null;
    if (overlay === null) {
      return;
    }
    panel.setAttribute("data-kind", overlay.kind);
    layer.setAttribute("data-kind", overlay.kind);
    clear(bodyEl);

    switch (overlay.kind) {
      case "help":
        titleEl.textContent = "Help";
        subtitleEl.textContent = "keys and gestures";
        layer.setAttribute("aria-label", "Help");
        append(bodyEl, helpBody(state));
        return;
      case "about":
        titleEl.textContent = "About FlightDeck";
        subtitleEl.textContent = "";
        layer.setAttribute("aria-label", "About FlightDeck");
        append(bodyEl, aboutBody(state));
        return;
      case "git_status":
        titleEl.textContent = "Git Status";
        /** SPECS §21 is explicit that the panel is *for the active Agent Tab*,
         * and the host names it — so the panel says which tab it is about
         * rather than leaving the reader to assume it is the one behind. */
        subtitleEl.textContent = overlay.panel.sessionName;
        layer.setAttribute("aria-label", "Git status");
        append(bodyEl, gitStatusBody(overlay.panel));
        return;
    }
  }

  return { el: layer, update: render };
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

function helpBody(state: AppState): Child[] {
  const host = state.help;
  const children: Child[] = [
    keyGroup(BROWSER_SECTION_TITLE, BROWSER_KEYS),
    el("div", { class: "fd-info__group-head" }, [
      el("span", {
        class: "fd-info__group-title",
        text: HOST_SECTION_TITLE,
      }),
      /**
       * D16's badge, on the heading rather than on every row. The badge exists
       * to be *noticed*; thirty of them down one column is wallpaper, and the
       * fact it carries — these act on the host's machine — is true of the
       * whole group uniformly, which is what a group heading is for.
       */
      hostOnlyBadge(),
    ]),
    el("p", { class: "fd-info__note", text: HOST_SECTION_NOTE }),
  ];

  if (host === null) {
    children.push(el("p", { class: "fd-info__absent", text: HOST_HELP_ABSENT }));
    return children;
  }

  /** The host's own title for its list, kept so nothing it sent is dropped. */
  children.push(el("p", { class: "fd-info__host-title", text: host.title }));

  /**
   * SPECS §32's isolated-run note leads the host's half, in the order the host
   * put it in — `help_doc` puts it first for both surfaces because the
   * desktop's overlay clips its own tail, and the browser rendering it in a
   * different place would be the two screens disagreeing about emphasis.
   */
  for (const note of host.notes) {
    children.push(
      el("div", { class: "fd-info__hostnote" }, [
        el("span", { class: "fd-info__hostnote-title", text: note.title }),
        ...note.lines.map((line) =>
          el("span", { class: "fd-info__hostnote-line", text: line }),
        ),
      ]),
    );
  }

  for (const section of host.sections) {
    children.push(keyGroup(section.title, section.rows));
  }
  return children;
}

/** One heading plus its rows, in 1d's group shape (a `--fd-focus` eyebrow over
 * left-aligned rows). Shared by both halves so the browser's keys and the
 * host's are visibly the same kind of list. */
function keyGroup(title: string, rows: readonly HelpRow[]): HTMLElement {
  return el("div", { class: "fd-info__group" }, [
    el("div", { class: "fd-info__group-head" }, [
      el("span", { class: "fd-info__group-title", text: title }),
    ]),
    ...rows.map((row) =>
      el("div", { class: "fd-info__key-row" }, [
        el("span", { class: "fd-key", text: row.keys }),
        el("span", { class: "fd-info__key-desc", text: row.description }),
      ]),
    ),
  ]);
}

// ---------------------------------------------------------------------------
// About
// ---------------------------------------------------------------------------

function aboutBody(state: AppState): Child[] {
  const about = state.about;
  if (about === null) {
    /** The host said nothing about itself, so this tab says nothing about the
     * host. Its own bundled version would be the wrong answer — D9 bakes the
     * SPA into the binary, so a tab left open across an update is running
     * *last* version's JavaScript against this version's server. */
    return [
      el("p", {
        class: "fd-info__absent",
        text: "This FlightDeck did not send its version or credits, so they are not shown.",
      }),
    ];
  }
  return [
    el("div", { class: "fd-info__about-name" }, [
      el("span", { class: "fd-info__about-product", text: about.name }),
      el("span", { class: "fd-info__about-version", text: `v${about.version}` }),
    ]),
    el("p", { class: "fd-info__about-tagline", text: about.tagline }),
    el(
      "div",
      { class: "fd-info__about-credits" },
      about.credits.map((credit) =>
        el("div", { class: "fd-info__about-credit" }, [
          el("span", { class: "fd-info__about-role", text: credit.role }),
          el("span", { class: "fd-info__about-person", text: credit.name }),
        ]),
      ),
    ),
    externalLink(about.url, about.url, "fd-info__about-url"),
  ];
}

// ---------------------------------------------------------------------------
// Git status (SPECS §21)
// ---------------------------------------------------------------------------

function gitStatusBody(panel: GitStatusPanel): Child[] {
  const rows: Child[] = [
    factRow("branch", [
      el("span", {
        class: "fd-tone-info",
        text: "⎇",
        attrs: { "aria-hidden": "true" },
      }),
      " ",
      el("span", { class: "fd-info__strong", text: panel.branch }),
    ]),
    factRow("base branch", [
      el("span", { class: "fd-info__strong", text: panel.baseBranch }),
    ]),
    /**
     * SPECS §12's drift, worded exactly as the desktop's own panel words it so
     * the two surfaces say the same sentence. `--fd-elsewhere` because 2g
     * names drift as its first meaning: another actor moved the base.
     */
    factRow(
      "base drift",
      panel.baseDrift === 0
        ? [el("span", { class: "fd-tone-ok", text: "none" })]
        : [
            el("span", {
              class: "fd-tone-elsewhere",
              text: `${panel.baseDrift} commits ahead since creation`,
            }),
          ],
    ),
    /**
     * `clean` in `--fd-ok`, as 1a's git bar already draws it. The dirty side
     * carries the count, because "dirty" alone is the one fact on this panel a
     * reader would immediately want a number for — and the number is the
     * host's, from the same porcelain read that set the flag.
     */
    factRow(
      "worktree",
      panel.dirty
        ? [
            el("span", {
              class: "fd-tone-focus",
              text: `dirty · ${panel.changedFiles} file${
                panel.changedFiles === 1 ? "" : "s"
              } uncommitted`,
            }),
          ]
        : [el("span", { class: "fd-tone-ok", text: "clean" })],
    ),
  ];

  /**
   * The unknown, rendered as the unknown. `no-upstream` is 2g's own example of
   * a fact that belongs in the **lifted** dim tier (`--fd-text-quiet`, 4.8:1)
   * rather than the decoration tier — deleting it would lose the fact that a
   * push has never happened, so it cannot be `--fd-text-decor`.
   */
  rows.push(
    factRow(
      "upstream",
      panel.upstream === null
        ? [
            el("span", {
              class: "fd-tone-quiet",
              text: "no-upstream",
              title: "this branch has never been pushed",
            }),
          ]
        : [el("span", { class: "fd-tone-accent", text: panel.upstream.name })],
    ),
  );

  /**
   * SPECS §21 asks for ahead/behind *"if known"*, and with no upstream it is
   * not known. The row is **absent**, not `↑0 ↓0` — which is what the host's
   * `WorktreeStatus` literally holds in that case, and forwarding it would be
   * the browser stating a measurement nobody took. The desktop's overlay omits
   * the same line for the same reason.
   */
  if (panel.upstream !== null) {
    rows.push(
      factRow("ahead / behind", [
        el("span", {
          class: "fd-tone-accent",
          text: `↑${panel.upstream.ahead} ↓${panel.upstream.behind}`,
          title: "commits ahead of and behind the upstream",
        }),
      ]),
    );
  }

  rows.push(
    factRow("worktree path", [
      el("span", { class: "fd-info__path", text: panel.worktreePath }),
      /** D16: a path on the machine running FlightDeck, which is not the
       * machine this browser is on. */
      hostOnlyBadge(),
    ]),
  );

  /**
   * SPECS §14's compare URL, and SPECS §5's boundary said out loud.
   *
   * The row is absent unless the host sent a URL — no placeholder, no
   * "not pushed yet" stand-in link, and above all nothing assembled here from
   * the branch name, which would invite the reader to a page that may not
   * exist. And where it *is* present, the caption states what FlightDeck did
   * and did not do: it pushed a branch and worked out where GitHub's compare
   * page is. **It did not open a pull request, and it never will** — §5 lists
   * "Create GitHub PRs" among the things FlightDeck must not do.
   */
  if (panel.compareUrl !== null) {
    rows.push(
      factRow("compare", [
        externalLink(panel.compareUrl, panel.compareUrl, "fd-info__link"),
      ]),
      el("p", {
        class: "fd-info__note",
        text: "Opens GitHub's compare page in a new tab. FlightDeck never creates the pull request itself (SPECS §5) — that is yours to do there.",
      }),
    );
  }

  return rows;
}

/** One `label · value` line, in 2f's fact-list shape: a quiet label in the
 * meta tier, the value at full contrast beside it. */
function factRow(label: string, value: readonly Child[]): HTMLElement {
  return el("div", { class: "fd-info__fact" }, [
    el("span", { class: "fd-info__fact-label", text: label }),
    el("span", { class: "fd-info__fact-value" }, value),
  ]);
}

/**
 * A link out of the app.
 *
 * `rel="noopener noreferrer"` and `target="_blank"`: the panel is a read-only
 * statement of facts, and following a link out of it must not take the session
 * with it — closing the FlightDeck tab to read a compare page would drop the
 * websocket and the seat along with it.
 */
function externalLink(href: string, text: string, className: string): HTMLElement {
  return el("a", {
    class: className,
    text,
    attrs: { href, target: "_blank", rel: "noopener noreferrer" },
  });
}
