import type { HostCommand, Project, Session } from "./model";
import type { AppState } from "./types";

/**
 * The command palette's inventory (artboard 1d) and the pure filtering /
 * highlighting / column logic it needs.
 *
 * ## Where the rows come from
 *
 * **The host, and only the host** (`remote-control-ll5.12`). `Snapshot::commands`
 * carries every name this build accepts, each row already wearing the label,
 * group, annotation, `host only` flag and — for a name the host will refuse
 * today — the exact sentence it refuses with (`src/web/commands.rs`'s
 * `INVENTORY`). `AppState.commands` is that list, and everything below renders
 * it.
 *
 * There is deliberately **no local inventory and no fallback**. The two rules
 * that buys us are worth stating, because they are the reason ll5.2's curated
 * list had to go:
 *
 * 1. *A row the host cannot execute cannot appear.* The browser has no way to
 *    offer a name this build does not implement, because it has no names of its
 *    own. Drop a row from the host's table and it leaves the palette with no
 *    change here.
 * 2. *A row the host **can** execute is not missing.* Ten commands open D13's
 *    shared dialog (`open_project`, `new_agent_session_tab`, `set_manual_status`
 *    …). Before this, no browser row sent any of them, so a browser could only
 *    ever answer a dialog the desktop had opened. Now every row the host lists
 *    is a row a browser can send.
 *
 * ## What this module still does
 *
 * Two things the host cannot do from where it sits:
 *
 * - **Expands the template rows.** A `target` (`project` / `session` /
 *   `terminal` / `unread_activity`) means "one row per target, with its id in
 *   `run.args`" — the host knows the shape, the browser knows the ids, and both
 *   halves come from host state either way (`AppState.projects`, `selection`,
 *   `activity`). A target kind this build does not recognise is skipped rather
 *   than sent without its argument.
 * - **Assigns each group to one of 1d's two columns.** Layout, not content.
 *
 * Everything else on a row is passed through untouched. In particular the
 * browser never words an annotation the host worded itself, and never
 * paraphrases a refusal.
 */

/**
 * One row of the palette: a [`HostCommand`] with its template expanded away.
 *
 * Field-for-field the host's row, minus `target` (spent by the expansion) and
 * minus `answersDialog` (those rows are not palette rows on either surface —
 * the dialog panel sends them). `run` is what gets sent, verbatim.
 */
export interface PaletteCommand {
  readonly id: string;
  readonly label: string;
  readonly group: string;
  readonly run: { readonly name: string; readonly args?: unknown };
  /** D16: stays visible, never hidden — see `hostOnlyBadge` in `ui/dom.ts`. */
  readonly hostOnly: boolean;
  /** 1d's right-hand tag (`destructive`, `next`, `current`, `3 unread`, …), or
   * `null` when neither the host nor the expansion has one to show. */
  readonly annotation: string | null;
  /** The host's own sentence for why it will refuse this row today, or `null`
   * when it runs it. Shown as-is; running the row returns the same words. */
  readonly refusal: string | null;
}

/**
 * Which of the palette's two columns (1d) a group renders in.
 *
 * Layout only — the group *names* are the host's. Column 0 is the selected
 * session's own surface (its sessions, terminals, tabs and worktree), column 1
 * is everything wider than it, which is how artboard 1d splits its four groups.
 * A group not listed here is a host that grew a new heading, not an error: it
 * lands in column 1 rather than disappearing.
 */
const GROUP_COLUMN: Readonly<Record<string, 0 | 1>> = {
  Sessions: 0,
  Terminals: 0,
  "Agent Session Tabs": 0,
  Worktree: 0,
  Projects: 1,
  Git: 1,
  Status: 1,
  Configuration: 1,
  Remote: 1,
  View: 1,
  Global: 1,
  Session: 1,
};

function columnOf(group: string): 0 | 1 {
  return GROUP_COLUMN[group] ?? 1;
}

/** The current project, or `null` before the first snapshot — mirrors
 * `findProject` in `model.ts` without importing it, so this module stays a
 * single self-contained read of `AppState`. */
function selectedProject(state: AppState): Project | null {
  if (state.selection === null) {
    return null;
  }
  return (
    state.projects.find((p) => p.id === state.selection?.projectId) ?? null
  );
}

function selectedSession(state: AppState, project: Project | null): Session | null {
  if (project === null || state.selection === null) {
    return null;
  }
  return (
    project.sessions.find((s) => s.id === state.selection?.sessionId) ?? null
  );
}

/**
 * The host's row, ready to render and send.
 *
 * `overrides` is what the expansion knows and the host does not: the target's
 * id in `run.args`, its name in the label, and — only where the host worded no
 * annotation of its own — the `current` / `N unread` tag. A host annotation is
 * never overwritten: it is the host's row, and this is the browser filling in
 * blanks, not editing prose.
 */
function paletteRow(
  row: HostCommand,
  overrides: {
    readonly idSuffix?: string;
    readonly labelSuffix?: string;
    readonly args?: unknown;
    readonly annotation?: string;
  } = {},
): PaletteCommand {
  return {
    id: overrides.idSuffix === undefined ? row.id : `${row.id}:${overrides.idSuffix}`,
    label:
      overrides.labelSuffix === undefined
        ? row.label
        : `${row.label}: ${overrides.labelSuffix}`,
    group: row.group,
    run:
      overrides.args === undefined
        ? row.run
        : { name: row.run.name, args: overrides.args },
    hostOnly: row.hostOnly,
    annotation: row.annotation ?? overrides.annotation ?? null,
    refusal: row.refusal,
  };
}

/**
 * One template row → one row per target.
 *
 * Every id and name below is read off `AppState`, which is host state the
 * browser was handed — nothing here is discovered locally. An empty result is a
 * correct one: no projects open means no `Switch to Project` rows, and nothing
 * unread means no `Mark All Activity Read` row at all.
 */
function expandTemplate(
  row: HostCommand,
  state: AppState,
): readonly PaletteCommand[] {
  const project = selectedProject(state);
  const session = selectedSession(state, project);

  switch (row.target) {
    case "project":
      return state.projects.map((p) =>
        paletteRow(row, {
          idSuffix: p.id,
          labelSuffix: p.name,
          args: { project_id: p.id },
          ...currentTag(p.id === project?.id),
        }),
      );
    case "session":
      return (project?.sessions ?? []).map((s) =>
        paletteRow(row, {
          idSuffix: s.id,
          labelSuffix: s.name,
          args: { session_id: s.id },
          ...currentTag(s.id === session?.id),
        }),
      );
    case "terminal":
      return (session?.terminals ?? []).map((t) =>
        paletteRow(row, {
          idSuffix: t.id,
          labelSuffix: t.title,
          args: { terminal_id: t.id },
          ...currentTag(t.id === state.selection?.terminalId),
        }),
      );
    case "unread_activity": {
      /** One row carrying every unread id, and no row at all when the feed is
       * clear — an offer to mark nothing read is not an offer. */
      const unread = state.activity.filter((event) => !event.read);
      if (unread.length === 0) {
        return [];
      }
      return [
        paletteRow(row, {
          args: { event_ids: unread.map((event) => event.id) },
          annotation: `${unread.length} unread`,
        }),
      ];
    }
    /** A target kind this build does not know how to fill in. Skipped rather
     * than rendered without its argument — the same call the host's own
     * `CommandTarget::Unrecognized` arm documents. */
    case "unrecognized":
      return [];
    case null:
      return [paletteRow(row)];
  }
}

/** `exactOptionalPropertyTypes` forbids `annotation: undefined` (the key must
 * be absent, not present-and-undefined), so this is spread rather than
 * assigned directly wherever "current" is conditional. */
function currentTag(isCurrent: boolean): { readonly annotation?: string } {
  return isCurrent ? { annotation: "current" } : {};
}

/**
 * The palette, in the host's own display order.
 *
 * Rebuilt from `state` on every open/filter — cheap, and it means a selection
 * made elsewhere is reflected the next time the palette is opened without a
 * second source of truth to keep in sync.
 *
 * Empty before the first snapshot, and empty against a host that sends no
 * inventory. Both are the honest answer: this browser does not know what that
 * host can run.
 */
export function paletteInventory(state: AppState): readonly PaletteCommand[] {
  const commands: PaletteCommand[] = [];
  for (const row of state.commands) {
    /** D13's two answers are not palette rows on either surface: the desktop
     * answers a dialog with its keyboard and the browser answers it with the
     * dialog panel's buttons (`ui/dialog.ts`). The host says which rows those
     * are, so there is no list of names here to drift. */
    if (row.answersDialog) {
      continue;
    }
    commands.push(...expandTemplate(row, state));
  }
  return commands;
}

/** One contiguous run of a label, matched or not — how `matchCommand` reports
 * *where* the filter hit so the row can render 2g's accent-coloured
 * substring without either side re-deriving the match. */
export interface LabelSpan {
  readonly text: string;
  readonly matched: boolean;
}

export interface MatchedCommand {
  readonly command: PaletteCommand;
  readonly labelSpans: readonly LabelSpan[];
}

/**
 * `null` when `filter` matches neither the label nor the annotation.
 *
 * 1d filters `wor` and highlights `Wor` inside `Worktree` — but also *matches*
 * `New Agent Session Tab` without highlighting anything in it, because the hit
 * is in that row's tag (`new worktree`). Searching both and only ever
 * highlighting the label is what reproduces that without a special case: a
 * label-only hit gets a highlighted span, an annotation-only hit gets a
 * command with no highlighted span at all.
 */
export function matchCommand(
  command: PaletteCommand,
  filter: string,
): MatchedCommand | null {
  const needle = filter.trim().toLowerCase();
  if (needle === "") {
    return { command, labelSpans: [{ text: command.label, matched: false }] };
  }
  const haystack = `${command.label} ${command.annotation ?? ""}`.toLowerCase();
  if (!haystack.includes(needle)) {
    return null;
  }
  const idx = command.label.toLowerCase().indexOf(needle);
  if (idx === -1) {
    return { command, labelSpans: [{ text: command.label, matched: false }] };
  }
  const spans: LabelSpan[] = [
    { text: command.label.slice(0, idx), matched: false },
    { text: command.label.slice(idx, idx + needle.length), matched: true },
    { text: command.label.slice(idx + needle.length), matched: false },
  ].filter((span) => span.text !== "");
  return { command, labelSpans: spans };
}

/** One column's groups, in inventory order, each with its surviving rows. */
export interface PaletteGroup {
  readonly name: string;
  readonly rows: readonly MatchedCommand[];
}

export interface PaletteColumns {
  /** Exactly two, per 1d — `columnOf` is the only place that assigns a group
   * to one, so a third never appears by accident. */
  readonly columns: readonly [readonly PaletteGroup[], readonly PaletteGroup[]];
  /** Flattened per column, in render order — what `↑↓`/`Tab` index into. */
  readonly flat: readonly [readonly MatchedCommand[], readonly MatchedCommand[]];
  readonly matchedCount: number;
  readonly totalCount: number;
}

export function paletteColumns(
  commands: readonly PaletteCommand[],
  filter: string,
): PaletteColumns {
  const groups = new Map<string, MatchedCommand[]>();
  let matchedCount = 0;
  for (const command of commands) {
    const matched = matchCommand(command, filter);
    if (matched === null) {
      continue;
    }
    matchedCount += 1;
    const bucket = groups.get(command.group);
    if (bucket === undefined) {
      groups.set(command.group, [matched]);
    } else {
      bucket.push(matched);
    }
  }

  const columnGroups: [PaletteGroup[], PaletteGroup[]] = [[], []];
  for (const [name, rows] of groups) {
    columnGroups[columnOf(name)].push({ name, rows });
  }

  const flat: [MatchedCommand[], MatchedCommand[]] = [[], []];
  for (const column of [0, 1] as const) {
    for (const group of columnGroups[column]) {
      flat[column].push(...group.rows);
    }
  }

  return {
    columns: columnGroups,
    flat,
    matchedCount,
    totalCount: commands.length,
  };
}

/** The command `↑↓`/`Enter` act on right now, or `null` when the active
 * column has no surviving row (an empty filter result, or a closed palette). */
export function highlightedCommand(
  state: AppState,
): PaletteCommand | null {
  const palette = state.palette;
  if (palette === null) {
    return null;
  }
  const { flat } = paletteColumns(paletteInventory(state), palette.filter);
  return flat[palette.column][palette.index]?.command ?? null;
}

/** Clamp an index into `[0, length - 1]`, or `0` for an empty list — never a
 * negative or out-of-range index for a component to render blindly. */
export function clampIndex(index: number, length: number): number {
  if (length === 0) {
    return 0;
  }
  return Math.max(0, Math.min(index, length - 1));
}
