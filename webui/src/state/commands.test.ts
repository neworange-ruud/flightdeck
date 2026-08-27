import { describe, expect, it } from "vitest";
import {
  buildCommandInventory,
  clampIndex,
  highlightedCommand,
  matchCommand,
  paletteColumns,
} from "./commands";
import { fixtureActivity, fixtureSnapshot } from "./fixture";
import { createInitialState } from "./types";
import type { AppState } from "./types";
import type { PaletteCommand } from "./commands";

/**
 * `buildCommandInventory` and the pure filter/highlight/column logic the
 * command palette (1d, `remote-control-ll5.2`) renders against. No DOM here —
 * see `ui/commandPalette.test.ts` for the rendered rows.
 */

function stateWithSnapshot(): AppState {
  const initial = createInitialState();
  const snapshot = fixtureSnapshot();
  return {
    ...initial,
    projects: snapshot.projects,
    selection: snapshot.selection,
    activity: snapshot.activity,
  };
}

describe("buildCommandInventory", () => {
  it("only ever lists commands this build can actually send", () => {
    const state = stateWithSnapshot();
    const commands = buildCommandInventory(state);
    expect(commands.length).toBeGreaterThan(0);
    for (const command of commands) {
      expect(command.run.name).not.toBe("");
    }
  });

  it("lists every session of the selected project, tagging the current one", () => {
    const state = stateWithSnapshot();
    const commands = buildCommandInventory(state);
    const project = state.projects.find((p) => p.id === state.selection?.projectId);
    expect(project).toBeDefined();

    const sessionRows = commands.filter((c) => c.group === "Sessions");
    expect(sessionRows).toHaveLength(project?.sessions.length ?? -1);

    const current = sessionRows.find(
      (c) => c.run.name === "select_session" &&
        (c.run.args as { session_id: string }).session_id === state.selection?.sessionId,
    );
    expect(current?.annotation).toBe("current");

    const other = sessionRows.find((c) => c !== current);
    expect(other?.annotation).toBeUndefined();
  });

  it("lists every project, and only the selected one as current", () => {
    const state = stateWithSnapshot();
    const commands = buildCommandInventory(state);
    const projectRows = commands.filter((c) => c.group === "Projects");
    expect(projectRows).toHaveLength(state.projects.length);
    expect(projectRows.filter((c) => c.annotation === "current")).toHaveLength(1);
  });

  it("lists the selected session's terminals, tagging the focused one", () => {
    const state = stateWithSnapshot();
    const commands = buildCommandInventory(state);
    const session = state.projects
      .find((p) => p.id === state.selection?.projectId)
      ?.sessions.find((s) => s.id === state.selection?.sessionId);
    const terminalRows = commands.filter((c) => c.group === "Terminals");
    expect(terminalRows).toHaveLength(session?.terminals.length ?? -1);
    expect(terminalRows.filter((c) => c.annotation === "current")).toHaveLength(1);
  });

  it("always offers Request Snapshot and Release Seat", () => {
    const state = stateWithSnapshot();
    const commands = buildCommandInventory(state);
    expect(commands.find((c) => c.run.name === "request_snapshot")).toBeDefined();
    expect(commands.find((c) => c.run.name === "release_seat")).toBeDefined();
  });

  it("offers Mark All Activity Read only when something is unread, carrying the real count", () => {
    const withUnread = stateWithSnapshot();
    const unreadCount = fixtureActivity().filter((e) => !e.read).length;
    expect(unreadCount).toBeGreaterThan(0);
    const markRead = buildCommandInventory(withUnread).find(
      (c) => c.run.name === "mark_activity_read",
    );
    expect(markRead?.annotation).toBe(`${unreadCount} unread`);
    expect(
      (markRead?.run.args as { event_ids: readonly string[] }).event_ids,
    ).toHaveLength(unreadCount);

    const allRead = {
      ...withUnread,
      activity: withUnread.activity.map((e) => ({ ...e, read: true })),
    };
    expect(
      buildCommandInventory(allRead).find((c) => c.run.name === "mark_activity_read"),
    ).toBeUndefined();
  });

  it("D16: still lists the two desktop-only actions, marked host-only", () => {
    const commands = buildCommandInventory(createInitialState());
    const fileManager = commands.find(
      (c) => c.label === "Open Worktree in File Manager",
    );
    const editor = commands.find((c) => c.label === "Edit in $EDITOR");
    expect(fileManager?.hostOnly).toBe(true);
    expect(editor?.hostOnly).toBe(true);
  });

  it("needs nothing selected yet to still offer the host-only and session actions", () => {
    // Before the first snapshot: no projects, no selection.
    const commands = buildCommandInventory(createInitialState());
    expect(commands.find((c) => c.run.name === "request_snapshot")).toBeDefined();
    expect(commands.find((c) => c.run.name === "release_seat")).toBeDefined();
    expect(commands.filter((c) => c.group === "Sessions")).toHaveLength(0);
    expect(commands.filter((c) => c.group === "Projects")).toHaveLength(0);
  });

  /**
   * `remote-control-ll5.7`. This is a real command this build can send today
   * — unlike the rest of the palette's `View`/`Worktree`/`Git` rows still
   * blocked on a future task, `toggle_split_view` exists on the wire
   * (`src/web/protocol.rs`'s `command` module). It carries no args and needs
   * no selection, so it is offered even before the first snapshot, exactly
   * like `request_snapshot`/`release_seat`.
   */
  it("offers Toggle Split View, with no args, even before the first snapshot", () => {
    const commands = buildCommandInventory(createInitialState());
    const toggle = commands.find((c) => c.id === "toggle_split_view");
    expect(toggle).toBeDefined();
    expect(toggle?.label).toBe("Toggle Split View");
    expect(toggle?.run).toEqual({ name: "toggle_split_view" });
    expect(toggle?.hostOnly).not.toBe(true);
  });

  it("Toggle Split View's annotation names the direction the toggle would take", () => {
    const single = { ...stateWithSnapshot(), layout: "single" as const };
    const split = { ...stateWithSnapshot(), layout: "split" as const };
    expect(
      buildCommandInventory(single).find((c) => c.id === "toggle_split_view")
        ?.annotation,
    ).toBe("single → split");
    expect(
      buildCommandInventory(split).find((c) => c.id === "toggle_split_view")
        ?.annotation,
    ).toBe("split → single");
  });
});

describe("matchCommand", () => {
  const command: PaletteCommand = {
    id: "select_session:s1",
    label: "Select Session: fix-login-redirect",
    group: "Sessions",
    run: { name: "select_session", args: { session_id: "s1" } },
  };

  it("matches everything, unhighlighted, when the filter is empty", () => {
    const matched = matchCommand(command, "");
    expect(matched?.labelSpans).toEqual([{ text: command.label, matched: false }]);
  });

  it("highlights the matched substring in the label, case-insensitively", () => {
    const matched = matchCommand(command, "SESSION");
    expect(matched?.labelSpans).toEqual([
      { text: "Select ", matched: false },
      { text: "Session", matched: true },
      { text: ": fix-login-redirect", matched: false },
    ]);
  });

  it("matches on the annotation without highlighting anything in the label", () => {
    const withAnnotation: PaletteCommand = {
      ...command,
      annotation: "3 unread",
    };
    const matched = matchCommand(withAnnotation, "unread");
    expect(matched).not.toBeNull();
    expect(matched?.labelSpans).toEqual([
      { text: withAnnotation.label, matched: false },
    ]);
  });

  it("is null when the filter hits neither the label nor the annotation", () => {
    expect(matchCommand(command, "nonexistent")).toBeNull();
  });
});

describe("paletteColumns", () => {
  it("counts matches against the whole inventory, not the filtered one", () => {
    const state = stateWithSnapshot();
    const commands = buildCommandInventory(state);
    const all = paletteColumns(commands, "");
    expect(all.matchedCount).toBe(commands.length);
    expect(all.totalCount).toBe(commands.length);

    const narrowed = paletteColumns(commands, "terminal");
    expect(narrowed.matchedCount).toBeLessThan(all.matchedCount);
    expect(narrowed.totalCount).toBe(commands.length);
  });

  it("puts session-scoped groups in column 0 and the rest in column 1", () => {
    const state = stateWithSnapshot();
    const commands = buildCommandInventory(state);
    const { columns } = paletteColumns(commands, "");
    const namesIn = (column: 0 | 1) => columns[column].map((g) => g.name);
    expect(namesIn(0)).toEqual(expect.arrayContaining(["Sessions", "Terminals"]));
    expect(namesIn(1)).toEqual(
      expect.arrayContaining(["Projects", "Session", "Worktree"]),
    );
    expect(namesIn(0)).not.toEqual(expect.arrayContaining(["Projects"]));
  });

  it("flattens each column in the same order its groups render", () => {
    const state = stateWithSnapshot();
    const commands = buildCommandInventory(state);
    const { columns, flat } = paletteColumns(commands, "");
    for (const column of [0, 1] as const) {
      const expected = columns[column].flatMap((g) => g.rows);
      expect(flat[column]).toEqual(expected);
    }
  });
});

describe("clampIndex", () => {
  it("clamps to 0 for an empty list", () => {
    expect(clampIndex(3, 0)).toBe(0);
    expect(clampIndex(-1, 0)).toBe(0);
  });

  it("clamps into range otherwise", () => {
    expect(clampIndex(-1, 5)).toBe(0);
    expect(clampIndex(99, 5)).toBe(4);
    expect(clampIndex(2, 5)).toBe(2);
  });
});

describe("highlightedCommand", () => {
  it("is null when the palette is closed", () => {
    expect(highlightedCommand(stateWithSnapshot())).toBeNull();
  });

  it("is the row at the open palette's column and index", () => {
    const state: AppState = {
      ...stateWithSnapshot(),
      palette: { filter: "", column: 0, index: 0, pending: [], lastOutcome: null },
    };
    const { flat } = paletteColumns(buildCommandInventory(state), "");
    expect(highlightedCommand(state)?.id).toBe(flat[0][0]?.command.id);
  });

  it("is null rather than throwing when the index has drifted out of range", () => {
    const state: AppState = {
      ...stateWithSnapshot(),
      palette: { filter: "", column: 0, index: 9999, pending: [], lastOutcome: null },
    };
    expect(highlightedCommand(state)).toBeNull();
  });
});
