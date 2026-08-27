import { findSession } from "../state/model";
import type { GitBarInfo } from "../state/model";
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
    bar.append(geometryChip(state.geometry));
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
    el("span", {}, [
      el("span", { class: "fd-tone-ok", text: `+${git.added}` }),
      " ",
      el("span", { class: "fd-tone-focus", text: `~${git.modified}` }),
      " ",
      el("span", { class: "fd-tone-alert", text: `-${git.removed}` }),
      " ",
      el("span", { class: "fd-decor", text: `(${git.files} files)` }),
    ]),
    separator(),
    el("span", {
      class: "fd-tone-accent",
      text: `↑${git.ahead} ↓${git.behind}`,
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

function geometryChip(geometry: TerminalGeometry | null): HTMLElement {
  const grid = geometry === null ? "—×—" : `${geometry.cols}×${geometry.rows}`;
  return el("span", {
    class: "fd-geometry",
    text: `${grid} · host owns geometry`,
    title:
      "the desktop owns the PTY grid (D4); the browser letterboxes it rather than scaling it, which is why a large window has dark margins",
  });
}
