import type { Project, Session } from "./model";
import type { AppState } from "./types";

/**
 * The command palette's inventory (artboard 1d) and the pure filtering /
 * highlighting / column logic it needs.
 *
 * ## Why this list is short
 *
 * Artboard 1d draws ~8 rows across two groups the design calls `Worktree` and
 * `Git` — `Rebase Worktree`, `Show Git Status`, `Push Branch`, and so on. Most
 * of those are still not commands this build can actually run: they belong to
 * the dialog family (`remote-control-ll5.3`/`.4`) or the git commands task
 * (`.5`), each a separate M2 task this one is explicitly told not to build.
 * `Toggle Split View` is the one exception — `remote-control-ll5.7` added it,
 * once `toggle_split_view` existed on the wire (`src/web/protocol.rs`'s
 * `command` module). Protocol v1 otherwise defines six command names:
 * `select_project`, `select_session`, `select_terminal`,
 * `mark_activity_read`, `request_snapshot`, `release_seat`.
 *
 * So `buildCommandInventory` below is built **only** from those six, plus the
 * two D16 desktop-only actions the spec names verbatim
 * (`Open Worktree in File Manager`, `e edit in $EDITOR`) — included so their
 * `host only` badge is real UI today, even though the host does not yet
 * recognise the command names this file had to invent for them
 * (`open_worktree_in_file_manager`, `edit_in_editor`). See `webui/README.md`
 * (or the ll5.2 task report) for exactly what `remote-control-ll5.1` needs to
 * supply so this module can stop inventing names and start reading a real
 * inventory off the snapshot.
 *
 * Every other row 1d draws — the full palette, tagged `destructive`,
 * `base: main`, `2m ago` — is `ll5.1`'s to add. This module's shape (a flat
 * `PaletteCommand[]`, one `run` frame each, an optional `hostOnly` flag, an
 * optional `annotation` string) is deliberately close to what a
 * `Snapshot::commands` field could hand over directly, so that swapping the
 * source is the same kind of change `remote-control-hgqy` made for the rest of
 * the app: a new source, not a new shape.
 */

/** One row of the palette. Every entry must be something this build can
 * actually send — see the module doc for why the list is short. */
export interface PaletteCommand {
  readonly id: string;
  readonly label: string;
  readonly group: string;
  readonly run: { readonly name: string; readonly args?: unknown };
  /** D16: stays visible, never hidden — see `hostOnlyBadge` in `ui/dom.ts`. */
  readonly hostOnly?: boolean;
  /** 1d's right-hand tag text (`base: main`, `2m ago`, …). Free text, because
   * once `ll5.1` supplies real commands this is theirs to word. */
  readonly annotation?: string;
}

/** Which of the palette's two columns (1d) a group renders in. A group not
 * listed here is an authoring mistake, not a runtime one — see the exhaustive
 * check in `columnOf`. */
const GROUP_COLUMN: Readonly<Record<string, 0 | 1>> = {
  Sessions: 0,
  Terminals: 0,
  Projects: 1,
  Worktree: 1,
  Session: 1,
  View: 1,
};

/**
 * The wire name for split-view toggling (`remote-control-ll5.1`'s
 * `command::TOGGLE_SPLIT_VIEW`, verified against `src/web/commands.rs` and
 * `src/web/protocol.rs`). Named once, here, so nothing else in this build
 * spells it out again — the same convention `state/config.ts`'s
 * `SAVE_CONFIG_COMMAND` uses for the one other cross-cutting command name.
 */
export const TOGGLE_SPLIT_VIEW_COMMAND = "toggle_split_view";

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

/** `exactOptionalPropertyTypes` forbids `annotation: undefined` (the key must
 * be absent, not present-and-undefined), so this is spread rather than
 * assigned directly wherever "current" is conditional. */
function currentTag(isCurrent: boolean): { readonly annotation?: string } {
  return isCurrent ? { annotation: "current" } : {};
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
 * The whole inventory this build can run, built fresh from `state` on every
 * open/filter — cheap, and it means a selection made elsewhere is reflected
 * the next time the palette is opened without a second source of truth to
 * keep in sync.
 */
export function buildCommandInventory(
  state: AppState,
): readonly PaletteCommand[] {
  const commands: PaletteCommand[] = [];
  const project = selectedProject(state);
  const session = selectedSession(state, project);

  if (project !== null) {
    for (const s of project.sessions) {
      commands.push({
        id: `select_session:${s.id}`,
        label: `Select Session: ${s.name}`,
        group: "Sessions",
        run: { name: "select_session", args: { session_id: s.id } },
        ...currentTag(s.id === session?.id),
      });
    }
  }

  for (const p of state.projects) {
    commands.push({
      id: `select_project:${p.id}`,
      label: `Switch to Project: ${p.name}`,
      group: "Projects",
      run: { name: "select_project", args: { project_id: p.id } },
      ...currentTag(p.id === project?.id),
    });
  }

  if (session !== null) {
    for (const terminal of session.terminals) {
      commands.push({
        id: `select_terminal:${terminal.id}`,
        label: `Select Terminal: ${terminal.title}`,
        group: "Terminals",
        run: { name: "select_terminal", args: { terminal_id: terminal.id } },
        ...currentTag(terminal.id === state.selection?.terminalId),
      });
    }
  }

  commands.push({
    id: "request_snapshot",
    label: "Request Snapshot",
    group: "Session",
    run: { name: "request_snapshot" },
    annotation: "resync from host",
  });

  commands.push({
    id: "release_seat",
    label: "Release Seat",
    group: "Session",
    run: { name: "release_seat" },
    annotation: "give up control",
  });

  /**
   * Artboard 1c/1d (`remote-control-ll5.7`). D3: split view is shared
   * instance state, so this is a `Command` like any other row here — the
   * host applies it (or refuses an observer with `ReadOnly`, D14) and
   * `AppState.layout` only ever moves from what comes back (`snapshot/received`,
   * `Delta::Selection`). This row never flips `layout` itself.
   */
  commands.push({
    id: "toggle_split_view",
    label: "Toggle Split View",
    group: "View",
    run: { name: TOGGLE_SPLIT_VIEW_COMMAND },
    annotation: state.layout === "split" ? "split → single" : "single → split",
  });

  const unread = state.activity.filter((event) => !event.read);
  if (unread.length > 0) {
    commands.push({
      id: "mark_activity_read",
      label: "Mark All Activity Read",
      group: "Session",
      run: {
        name: "mark_activity_read",
        args: { event_ids: unread.map((event) => event.id) },
      },
      annotation: `${unread.length} unread`,
    });
  }

  /**
   * Artboard 1f (`remote-control-ll5.6`). SPECS §8: "command palette → 'Open
   * Configuration'" is how the manager opens. Unlike every other row here,
   * `run` is never sent anywhere — `ui/app.ts`'s `runCommand` special-cases
   * this `id` and dispatches `config/open` locally, the same way `Ctrl-g`
   * opens *this* palette without a `Command` frame of its own. `run.name`
   * exists only so this stays a normal `PaletteCommand`; it is never read.
   */
  commands.push({
    id: "open_configuration",
    label: "Open Configuration",
    group: "Session",
    run: { name: "open_configuration" },
    annotation: "global + project settings",
  });

  /**
   * D16. `run.name` is a placeholder — protocol v1 has no command by this
   * name yet. See the module doc: `remote-control-ll5.1` owns adding it, and
   * the badge stays visible and honest in the meantime rather than hiding a
   * desktop-only action the design requires to be shown.
   */
  commands.push({
    id: "open_worktree_in_file_manager",
    label: "Open Worktree in File Manager",
    group: "Worktree",
    run: { name: "open_worktree_in_file_manager" },
    hostOnly: true,
  });
  commands.push({
    id: "edit_in_editor",
    label: "Edit in $EDITOR",
    group: "Worktree",
    run: { name: "edit_in_editor" },
    hostOnly: true,
  });

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
  const inventory = buildCommandInventory(state);
  const { flat } = paletteColumns(inventory, palette.filter);
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
