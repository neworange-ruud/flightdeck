import { describe, expect, it } from "vitest";
import {
  clampIndex,
  highlightedCommand,
  matchCommand,
  paletteColumns,
  paletteInventory,
} from "./commands";
import { fixtureActivity, fixtureCommands, fixtureSnapshot } from "./fixture";
import { createInitialState } from "./types";
import type { AppState } from "./types";
import type { PaletteCommand } from "./commands";
import type { HostCommand } from "./model";

/**
 * `paletteInventory` and the pure filter/highlight/column logic the command
 * palette (1d) renders against. No DOM here — see `ui/commandPalette.test.ts`
 * for the rendered rows.
 *
 * Everything below leans on one fact (`remote-control-ll5.12`): the rows come
 * from `AppState.commands`, which is `Snapshot::commands`, which is the host's
 * `INVENTORY`. So the tests assert against `fixtureCommands()` — the recorded
 * host payload — rather than against a list this file agrees with the module
 * about.
 */

function stateWithSnapshot(): AppState {
  const initial = createInitialState();
  const snapshot = fixtureSnapshot();
  return {
    ...initial,
    projects: snapshot.projects,
    selection: snapshot.selection,
    activity: snapshot.activity,
    commands: snapshot.commands,
  };
}

/** A state carrying exactly the host rows given, and nothing else — for the
 * cases where the point is what the *host* listed. */
function stateWithCommands(commands: readonly HostCommand[]): AppState {
  return { ...stateWithSnapshot(), commands };
}

/** The host row with this id, from the recorded payload. */
function hostRow(id: string): HostCommand {
  const row = fixtureCommands().find((c) => c.id === id);
  if (row === undefined) {
    throw new Error(`no host row '${id}' in the recorded inventory`);
  }
  return row;
}

describe("paletteInventory: the host's list, and only the host's list", () => {
  it("offers nothing at all before the first snapshot", () => {
    /** Not a degraded state to paper over: this browser has not been told
     * what that host implements, and inventing rows is exactly the bug the
     * host-sent inventory exists to prevent. */
    expect(paletteInventory(createInitialState())).toEqual([]);
  });

  it("offers nothing against a host that sends an empty inventory", () => {
    expect(paletteInventory(stateWithCommands([]))).toEqual([]);
  });

  it("every row it offers is a row the host listed", () => {
    const names = new Set(fixtureCommands().map((c) => c.run.name));
    const commands = paletteInventory(stateWithSnapshot());
    expect(commands.length).toBeGreaterThan(0);
    for (const command of commands) {
      expect(names.has(command.run.name)).toBe(true);
    }
  });

  /**
   * The property the whole task turns on: the palette has no list of its own,
   * so dropping a row from what the host sends drops it from the palette with
   * no change to any browser code.
   */
  it("a row the host stops sending stops being offered, with no code change", () => {
    const full = stateWithSnapshot();
    expect(
      paletteInventory(full).some((c) => c.run.name === "open_shell"),
    ).toBe(true);

    const trimmed = stateWithCommands(
      full.commands.filter((c) => c.id !== "open_shell"),
    );
    expect(
      paletteInventory(trimmed).some((c) => c.run.name === "open_shell"),
    ).toBe(false);
    /** And nothing else moved: it is one row fewer, not a different list. */
    expect(paletteInventory(trimmed)).toHaveLength(
      paletteInventory(full).length - 1,
    );
  });

  it("offers a row this file has never heard of, if the host sends one", () => {
    const invented: HostCommand = {
      id: "teleport_worktree",
      label: "Teleport Worktree",
      group: "Worktree",
      run: { name: "teleport_worktree" },
      hostOnly: false,
      answersDialog: false,
      annotation: "brand new",
      target: null,
      refusal: null,
    };
    const row = paletteInventory(stateWithCommands([invented]))[0];
    expect(row?.label).toBe("Teleport Worktree");
    expect(row?.annotation).toBe("brand new");
  });

  /** D13: the desktop answers a dialog with its keyboard and the browser
   * answers it with the dialog panel's buttons. The host says which rows those
   * are (`answers_dialog`), so the palette leaves them out without a list of
   * names of its own. */
  it("leaves out the two rows the host flags as answering the open dialog", () => {
    const commands = paletteInventory(stateWithSnapshot());
    expect(commands.find((c) => c.run.name === "dialog_confirm")).toBeUndefined();
    expect(commands.find((c) => c.run.name === "dialog_cancel")).toBeUndefined();
    /** …and the host really did send them, so this is a filter doing work. */
    expect(hostRow("dialog_confirm").answersDialog).toBe(true);
    expect(hostRow("dialog_cancel").answersDialog).toBe(true);
  });

  it("keeps the host's display order, template expansions in place", () => {
    const commands = paletteInventory(stateWithSnapshot());
    const firstNames = commands.slice(0, 3).map((c) => c.run.name);
    /** `select_session` is the host's first row and expands over the selected
     * project's sessions, so the first several rows are all that one name. */
    expect(firstNames).toEqual([
      "select_session",
      "select_session",
      "select_session",
    ]);
  });
});

describe("target templates expand over state the browser already has", () => {
  it("session: one row per session of the selected project, current tagged", () => {
    const state = stateWithSnapshot();
    const project = state.projects.find((p) => p.id === state.selection?.projectId);
    expect(project).toBeDefined();

    const rows = paletteInventory(state).filter(
      (c) => c.run.name === "select_session",
    );
    expect(rows).toHaveLength(project?.sessions.length ?? -1);
    expect(rows[0]?.label).toBe(`Select Session: ${project?.sessions[0]?.name}`);
    expect(rows[0]?.group).toBe("Sessions");

    const current = rows.find(
      (c) =>
        (c.run.args as { session_id: string }).session_id ===
        state.selection?.sessionId,
    );
    expect(current?.annotation).toBe("current");
    expect(rows.filter((c) => c.annotation === "current")).toHaveLength(1);
  });

  it("project: one row per open project, only the selected one current", () => {
    const state = stateWithSnapshot();
    const rows = paletteInventory(state).filter(
      (c) => c.run.name === "select_project",
    );
    expect(rows).toHaveLength(state.projects.length);
    expect(rows[0]?.run.args).toEqual({ project_id: state.projects[0]?.id });
    expect(rows[0]?.label).toBe(`Switch to Project: ${state.projects[0]?.name}`);
    expect(rows.filter((c) => c.annotation === "current")).toHaveLength(1);
  });

  it("terminal: one row per terminal of the selected session", () => {
    const state = stateWithSnapshot();
    const session = state.projects
      .find((p) => p.id === state.selection?.projectId)
      ?.sessions.find((s) => s.id === state.selection?.sessionId);
    const rows = paletteInventory(state).filter(
      (c) => c.run.name === "select_terminal",
    );
    expect(rows).toHaveLength(session?.terminals.length ?? -1);
    expect(rows[0]?.run.args).toEqual({
      terminal_id: session?.terminals[0]?.id,
    });
    expect(rows.filter((c) => c.annotation === "current")).toHaveLength(1);
  });

  it("unread_activity: one row carrying every unread id, and the real count", () => {
    const state = stateWithSnapshot();
    const unreadCount = fixtureActivity().filter((e) => !e.read).length;
    expect(unreadCount).toBeGreaterThan(0);

    const row = paletteInventory(state).find(
      (c) => c.run.name === "mark_activity_read",
    );
    expect(row?.label).toBe("Mark All Activity Read");
    expect(row?.annotation).toBe(`${unreadCount} unread`);
    expect(
      (row?.run.args as { event_ids: readonly string[] }).event_ids,
    ).toHaveLength(unreadCount);
  });

  it("unread_activity: no row at all when nothing is unread", () => {
    const state = stateWithSnapshot();
    const allRead = {
      ...state,
      activity: state.activity.map((e) => ({ ...e, read: true })),
    };
    expect(
      paletteInventory(allRead).find((c) => c.run.name === "mark_activity_read"),
    ).toBeUndefined();
  });

  it("expands to nothing, rather than to an argument-less row, with no targets", () => {
    /** Before the first snapshot there are no projects and no selection, so
     * the three selection templates have nothing to expand over. A row with no
     * `session_id` would be a frame the host must refuse. */
    const bare = stateWithCommands(fixtureCommands());
    const empty: AppState = {
      ...bare,
      projects: [],
      selection: null,
      activity: [],
    };
    const commands = paletteInventory(empty);
    for (const name of [
      "select_session",
      "select_project",
      "select_terminal",
      "mark_activity_read",
    ]) {
      expect(commands.find((c) => c.run.name === name)).toBeUndefined();
    }
    /** The rows that need no target are still there. */
    expect(commands.find((c) => c.run.name === "request_snapshot")).toBeDefined();
  });

  it("skips a target kind this build does not recognise", () => {
    /** The host's own `CommandTarget::Unrecognized` arm reaching the browser:
     * the row cannot be filled in, so it is not offered rather than sent
     * without its argument. */
    const unknown: HostCommand = {
      ...hostRow("select_session"),
      id: "select_pod",
      run: { name: "select_pod" },
      target: "unrecognized",
    };
    expect(paletteInventory(stateWithCommands([unknown]))).toEqual([]);
  });

  it("never overwrites an annotation the host worded itself", () => {
    const annotated: HostCommand = {
      ...hostRow("select_project"),
      annotation: "shared selection",
    };
    const rows = paletteInventory(stateWithCommands([annotated]));
    expect(rows.length).toBeGreaterThan(1);
    for (const row of rows) {
      expect(row.annotation).toBe("shared selection");
    }
  });
});

describe("D16 and refusals come from the host, never from here", () => {
  it("carries the host's host_only flag on exactly the rows it flagged", () => {
    const commands = paletteInventory(stateWithSnapshot());
    const badged = commands.filter((c) => c.hostOnly).map((c) => c.run.name);
    const hostBadged = fixtureCommands()
      .filter((c) => c.hostOnly)
      .map((c) => c.run.name);
    expect(badged).toEqual(hostBadged);
    expect(badged).toEqual(["open_worktree_in_file_manager", "edit_in_editor"]);
  });

  it("carries the host's refusal sentence verbatim", () => {
    const commands = paletteInventory(stateWithSnapshot());
    const badged = commands.find(
      (c) => c.run.name === "open_worktree_in_file_manager",
    );
    expect(badged?.refusal).toBe(hostRow("open_worktree_in_file_manager").refusal);
    expect(badged?.refusal).toContain("machine this browser is on");
  });

  it("carries the host's annotation on a destructive row it does run", () => {
    /** `abandon_worktree` and `quit` stopped being refusals in
     * `remote-control-ll5.4` — the second step moved onto the dialog they open
     * (§6.5 R13). The tag beside them is still the host's word, and the browser
     * did not invent a refusal to replace the one that went away. */
    const commands = paletteInventory(stateWithSnapshot());
    for (const name of ["abandon_worktree", "quit"]) {
      const row = commands.find((c) => c.run.name === name);
      expect(row?.annotation).toBe("destructive");
      expect(row?.refusal).toBeNull();
    }
  });

  it("a row the host runs carries no refusal", () => {
    const commands = paletteInventory(stateWithSnapshot());
    expect(commands.find((c) => c.run.name === "open_shell")?.refusal).toBeNull();
  });

  it("a refused row is still offered — visible and honest beats hidden", () => {
    const commands = paletteInventory(stateWithSnapshot());
    for (const name of ["pull_base", "show_help", "pair_phone"]) {
      const row = commands.find((c) => c.run.name === name);
      expect(row).toBeDefined();
      expect(row?.refusal).not.toBeNull();
    }
  });

  /** `show_git_status` stopped being a refusal in `remote-control-ll5.8`
   * (§6.5 R16): the host runs SPECS §21's collection and answers this tab with
   * the panel, so the row carries no refusal and keeps artboard 1d's own tag. */
  it("show_git_status runs now, and keeps 1d's tag", () => {
    const commands = paletteInventory(stateWithSnapshot());
    const row = commands.find((c) => c.run.name === "show_git_status");
    expect(row?.refusal).toBeNull();
    expect(row?.annotation).toBe("worktree detail");
  });
});

/**
 * The git surface (`remote-control-ll5.5`; SPECS §5).
 *
 * Nothing in this module changed for it — that is the assertion. The host
 * flipped four rows in its own table and the palette followed, because the
 * palette has no git knowledge of its own to update: no list of runnable names,
 * no idea which command touches history, no local copy of §5's boundary. So
 * these tests read the recorded host payload and check the browser passed it
 * through, which is the only correct thing for a browser to do with it.
 */
describe("the git rows are the host's answer, not the browser's", () => {
  it("offers the three the host runs, with no refusal to show", () => {
    const commands = paletteInventory(stateWithSnapshot());
    for (const name of ["rebase_worktree", "push_branch", "finish_local_merge"]) {
      const row = commands.find((c) => c.run.name === name);
      expect(row, `no palette row sends '${name}'`).toBeDefined();
      expect(row?.refusal, `'${name}' would be refused`).toBeNull();
      /** A bare frame: SPECS §5's confirmation flags live in the host's table,
       * never in `args` a browser could fill in. */
      expect(row?.run.args).toBeUndefined();
    }
  });

  it("shows Pull Base with the host's boundary decision, not a hidden row", () => {
    const commands = paletteInventory(stateWithSnapshot());
    const pullBase = commands.find((c) => c.run.name === "pull_base");
    expect(pullBase).toBeDefined();
    expect(pullBase?.refusal).toBe(hostRow("pull_base").refusal);
    expect(pullBase?.refusal).toContain("SPECS §5.2");
  });

  it("would run Pull Base too, the day the host stops refusing it", () => {
    /** The proof that the refusal is the host's position and not this file's:
     * the same row with `refusal: null` is offered exactly like the other
     * three, with nothing here to change. */
    const host = hostRow("pull_base");
    const commands = paletteInventory(
      stateWithCommands([{ ...host, refusal: null }]),
    );
    expect(commands[0]?.run.name).toBe("pull_base");
    expect(commands[0]?.refusal).toBeNull();
  });
});

/**
 * The bug `remote-control-ll5.12` fixes: ten commands open D13's shared dialog
 * on the host, and before this no browser row sent any of them — so a browser
 * could only ever answer a dialog the desktop had opened.
 */
describe("a browser can open every dialog the host offers", () => {
  const DIALOG_OPENERS = [
    "open_project",
    "close_project",
    "new_agent_session_tab",
    "rename_agent_session_tab",
    "close_agent_session_tab",
    "close_child_terminal",
    "new_agent",
    "close_agent",
    "set_manual_status",
    "unpair_phone",
  ];

  it("offers a sendable row for each of the ten, none of them refused", () => {
    const commands = paletteInventory(stateWithSnapshot());
    for (const name of DIALOG_OPENERS) {
      const row = commands.find((c) => c.run.name === name);
      expect(row, `no palette row sends '${name}'`).toBeDefined();
      expect(row?.run.name).toBe(name);
      /** The host dispatches these through its own palette path — a refusal
       * here would mean the dialog never opens. */
      expect(row?.refusal, `'${name}' would be refused`).toBeNull();
    }
  });

  it("sends them with no args, because the dialog collects what it needs", () => {
    const commands = paletteInventory(stateWithSnapshot());
    for (const name of DIALOG_OPENERS) {
      expect(commands.find((c) => c.run.name === name)?.run.args).toBeUndefined();
    }
  });
});

describe("matchCommand", () => {
  const command: PaletteCommand = {
    id: "select_session:s1",
    label: "Select Session: fix-login-redirect",
    group: "Sessions",
    run: { name: "select_session", args: { session_id: "s1" } },
    hostOnly: false,
    annotation: null,
    refusal: null,
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

  it("reproduces artboard 1d's `wor` query against the host's own rows", () => {
    /** 1d types `wor` and lists `New Agent Session Tab` with **nothing
     * highlighted in its label**, because the hit is in that row's tag. That
     * only works if the host puts the tag there, and until §6.5 R19 it sent
     * `annotation: null` — a doc comment describing behaviour the data could
     * not produce. This asserts the data, through the recorded host payload. */
    const rows = paletteInventory(stateWithSnapshot());
    const newTab = rows.find((c) => c.run.name === "new_agent_session_tab");
    expect(newTab?.annotation).toBe("new worktree");

    const matched = matchCommand(newTab as PaletteCommand, "wor");
    expect(matched).not.toBeNull();
    expect(matched?.labelSpans).toEqual([
      { text: "New Agent Session Tab", matched: false },
    ]);

    /** And the label-side half of the same query still highlights. */
    const abandon = rows.find((c) => c.run.name === "abandon_worktree");
    expect(matchCommand(abandon as PaletteCommand, "wor")?.labelSpans).toEqual([
      { text: "Abandon ", matched: false },
      { text: "Wor", matched: true },
      { text: "ktree", matched: false },
    ]);
  });
});

describe("paletteColumns", () => {
  it("counts matches against the whole inventory, not the filtered one", () => {
    const commands = paletteInventory(stateWithSnapshot());
    const all = paletteColumns(commands, "");
    expect(all.matchedCount).toBe(commands.length);
    expect(all.totalCount).toBe(commands.length);

    const narrowed = paletteColumns(commands, "terminal");
    expect(narrowed.matchedCount).toBeLessThan(all.matchedCount);
    expect(narrowed.totalCount).toBe(commands.length);
  });

  it("puts the session's own groups in column 0 and the wider ones in column 1", () => {
    const commands = paletteInventory(stateWithSnapshot());
    const { columns } = paletteColumns(commands, "");
    const namesIn = (column: 0 | 1) => columns[column].map((g) => g.name);
    expect(namesIn(0)).toEqual(
      expect.arrayContaining([
        "Sessions",
        "Terminals",
        "Agent Session Tabs",
        "Worktree",
      ]),
    );
    expect(namesIn(1)).toEqual(
      expect.arrayContaining(["Projects", "Git", "View", "Session"]),
    );
    expect(namesIn(0)).not.toEqual(expect.arrayContaining(["Projects"]));
  });

  it("puts a group heading the host invented in column 1 rather than dropping it", () => {
    const invented: HostCommand = {
      ...hostRow("open_shell"),
      id: "warp_drive",
      group: "Warp",
      run: { name: "warp_drive" },
    };
    const { columns } = paletteColumns(
      paletteInventory(stateWithCommands([invented])),
      "",
    );
    expect(columns[1].map((g) => g.name)).toEqual(["Warp"]);
  });

  it("flattens each column in the same order its groups render", () => {
    const commands = paletteInventory(stateWithSnapshot());
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
    const { flat } = paletteColumns(paletteInventory(state), "");
    expect(highlightedCommand(state)?.id).toBe(flat[0][0]?.command.id);
  });

  it("is null rather than throwing when the index has drifted out of range", () => {
    const state: AppState = {
      ...stateWithSnapshot(),
      palette: { filter: "", column: 0, index: 9999, pending: [], lastOutcome: null },
    };
    expect(highlightedCommand(state)).toBeNull();
  });

  it("is null against a host that sent no inventory, rather than throwing", () => {
    const state: AppState = {
      ...stateWithCommands([]),
      palette: { filter: "", column: 0, index: 0, pending: [], lastOutcome: null },
    };
    expect(highlightedCommand(state)).toBeNull();
  });
});
