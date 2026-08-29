import { findSession } from "../state/model";
import type { GitBarInfo } from "../state/model";
import type { ViewportWidth } from "../state/viewport";
import type { AppState, TerminalGeometry } from "../state/types";
import { clear, el, separator } from "./dom";
import type { Child, Region } from "./dom";

/**
 * Region 6 of 7 — the git info bar (1a, above the status bar).
 *
 * Dim-tier audit for this row, since it is where most of the app's facts live:
 *   - the branch name, `+3 ~2 -1`, `↑3 ↓0`, `base +4` and `base: main` are all
 *     facts. The first four were already coloured by meaning; `base: main` is
 *     the one 1a drew at the decoration tier, and it fails 2g's test — delete
 *     it and you no longer know what `base +4` counts from, so it is lifted to
 *     `--fd-text-quiet`.
 *   - `(6 files)` stays `--fd-text-decor`: 2g names "item counts" as
 *     decoration, and the count adds no fact the three numbers beside it do not
 *     already carry.
 *   - `no-upstream` replaces the ahead/behind pair when there is no upstream,
 *     and §5.1 names it as a fact — so `--fd-text-quiet`, exactly where the
 *     sidebar row and the git-status panel already put the same word.
 *
 * And the geometry chip, which D4 calls out by name as *not decoration*: it is
 * the honest explanation for why a large browser window has dark margins. It
 * shows the host's `cols×rows` verbatim — never the browser's own measurement,
 * because the browser does not have one.
 */
export function createGitBar(): Region {
  const bar = el("div", {
    class: "fd-gitbar",
    attrs: { "aria-label": "Git status" },
  });

  function render(state: AppState): void {
    clear(bar);
    const selection = state.selection;
    const session =
      selection === null
        ? null
        : findSession(state.projects, selection.projectId, selection.sessionId);

    const parts: Child[] =
      session?.gitBar != null
        ? gitParts(session.gitBar)
        : [
            el("span", {
              class: "fd-tone-quiet",
              text: "git: ?",
              title: "git has not answered for this worktree yet",
            }),
          ];

    for (const part of parts) {
      if (part !== null && part !== undefined && part !== false) {
        bar.append(part);
      }
    }
    bar.append(geometryChip(state.geometry, state.width));
  }

  return { el: bar, update: render };
}

function gitParts(git: GitBarInfo): Child[] {
  return [
    el("span", {
      class: "fd-tone-info",
      text: "⎇",
      attrs: { "aria-hidden": "true" },
    }),
    el("span", { class: "fd-gitbar__branch", text: git.branch }),
    separator(),
    changeCounts(git),
    separator(),
    /**
     * The counts, or the reason there are none. A branch with no upstream has
     * nothing to be ahead of, so the bar says `no-upstream` — the same fact the
     * sidebar row states from the same bool, at the same lifted tier (§5.1
     * names it), rather than the `↑0 ↓0` the two used to contradict each other
     * with (§6.5 R23). The type is what makes that unrepresentable; this is
     * only the rendering of it.
     */
    git.upstream === null
      ? el("span", {
          class: "fd-tone-quiet",
          text: "no-upstream",
          title: "this branch has no upstream — a push would have nowhere to go",
        })
      : el("span", {
          class: "fd-tone-accent",
          text: `↑${git.upstream.ahead} ↓${git.upstream.behind}`,
          title: "commits ahead of and behind the upstream",
        }),
    separator(),
    el("span", {
      class: "fd-tone-elsewhere",
      text: `base +${git.baseAhead}`,
      title: "commits the base branch has moved on by",
    }),
    separator(),
    el("span", { class: "fd-tone-quiet", text: `base: ${git.base}` }),
  ];
}

/**
 * `+3 ~2 -1 (6 files)`, or 2e's one word for the same worktree with nothing in
 * it. 2e's git bar reads `⎇ branch │ clean │ 120×34 · host owns geometry`, and
 * `+0 ~0 -0 (0 files)` is four numbers that say what one word says better — the
 * same call the git-status panel already makes (`infoOverlay.ts`) and the same
 * predicate the host already spells out (`GitBar::is_clean`, whose own doc says
 * it "renders `clean`"), so every surface words a clean worktree identically.
 */
function changeCounts(git: GitBarInfo): HTMLElement {
  if (git.added === 0 && git.modified === 0 && git.removed === 0) {
    return el("span", { class: "fd-tone-ok", text: "clean" });
  }
  return el("span", {}, [
    el("span", { class: "fd-tone-ok", text: `+${git.added}` }),
    " ",
    el("span", { class: "fd-tone-focus", text: `~${git.modified}` }),
    " ",
    el("span", { class: "fd-tone-alert", text: `-${git.removed}` }),
    " ",
    el("span", { class: "fd-decor", text: `(${git.files} files)` }),
  ]);
}

/**
 * D4's chip, and below 900px D4's *other* half.
 *
 * At wide the chip answers "why does my 4K window have dark margins?". At
 * narrow the interesting question inverts — "why is my terminal wider than my
 * screen?" — and the answer is the same decision read the other way round:
 * the host owns the grid, so a viewport smaller than it **scrolls, and is
 * never scaled to fit**. So the chip gains that clause below 900px, where the
 * question is live (`remote-control-eek.4`, §6.5 R17).
 *
 * It is a *statement of policy*, true whether or not this particular grid
 * happens to overflow this particular viewport — deliberately, because the
 * alternative is for the browser to measure itself, and D4's whole position is
 * that the browser never negotiates or requests a size, it only receives one.
 * Measuring the stage to decide what to print would be the first step back
 * towards a `FitAddon`, so this file does not do it and the *scrollbar* is
 * what says the grid is actually over the edge right now.
 */
function geometryChip(
  geometry: TerminalGeometry | null,
  width: ViewportWidth,
): HTMLElement {
  const grid = geometry === null ? "—×—" : `${geometry.cols}×${geometry.rows}`;
  const scrolls = width === "narrow";
  return el("span", {
    class: "fd-geometry",
    text: `${grid} · host owns geometry${scrolls ? " · scroll, never scale" : ""}`,
    title:
      "the desktop owns the PTY grid (D4); the browser letterboxes it rather than scaling it, which is why a large window has dark margins — and why a window narrower than the grid scrolls instead of shrinking it",
  });
}
