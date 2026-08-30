import { describe, expect, it } from "vitest";
import {
  isLoopbackAddress,
  nextConfigValue,
  NO_CONFIG_EDITS,
  resolveConfigRow,
  ROUTABLE_BIND_WARNING,
  stagedChanges,
  stagedCount,
} from "./config";
import type { ConfigDoc, ConfigEdits, ConfigRow } from "./config";

/**
 * The staged-edit arithmetic, in isolation from the DOM — see
 * `wire/wiredConfig.test.ts` for the panel itself, driven by real frames.
 *
 * These are the only functions in the configuration manager that *decide*
 * anything in the browser, and each of them decides from a fact the host sent:
 * `resolveConfigRow` swaps in the host's own `inherited` for a staged clear,
 * and `nextConfigValue` advances through the host's own `choices`. There is no
 * field list and no layer walk here to test, because there is none to have —
 * that is what `remote-control-1p22` removed.
 */

function boolRow(over: Partial<ConfigRow> = {}): ConfigRow {
  return {
    key: "notifications.on_finish",
    label: "Notify when finished",
    kind: "bool",
    value: true,
    choices: [],
    origin: "set_here",
    inherited: false,
    inheritedOrigin: "global",
    ...over,
  };
}

function choiceRow(): ConfigRow {
  return {
    key: "ui.mode_border",
    label: "Mode border",
    kind: "choice",
    value: "dim",
    choices: ["off", "dim", "normal", "bright"],
    origin: "default",
    inherited: "off",
    inheritedOrigin: "default",
  };
}

function textRow(value: string): ConfigRow {
  return {
    key: "web.bind",
    label: "Web interface bind address",
    kind: "text",
    value,
    choices: [],
    origin: "default",
    inherited: "127.0.0.1",
    inheritedOrigin: "default",
  };
}

function edits(scope: "global" | "project", key: string, edit: unknown): ConfigEdits {
  return {
    ...NO_CONFIG_EDITS,
    [scope]: { [key]: edit },
  } as ConfigEdits;
}

describe("resolveConfigRow", () => {
  it("passes the host's answer through untouched when nothing is staged", () => {
    const resolved = resolveConfigRow(boolRow(), "project", NO_CONFIG_EDITS);
    expect(resolved.value).toBe(true);
    expect(resolved.origin).toBe("set_here");
    expect(resolved.staged).toBe(false);
  });

  it("a staged set reads `set here`, whatever the value", () => {
    /** Setting a value in a scope *is* what "set here" means — including
     * setting it to the same value the layer below already had, which is why
     * this is never decided by comparing values. */
    const resolved = resolveConfigRow(
      boolRow({ origin: "global", value: false }),
      "project",
      edits("project", "notifications.on_finish", { kind: "set", value: false }),
    );
    expect(resolved.value).toBe(false);
    expect(resolved.origin).toBe("set_here");
    expect(resolved.staged).toBe(true);
  });

  it("a staged clear reads the host's own inherited value and tag", () => {
    const resolved = resolveConfigRow(
      boolRow(),
      "project",
      edits("project", "notifications.on_finish", { kind: "clear" }),
    );
    expect(resolved.value).toBe(false);
    expect(resolved.origin).toBe("global");
  });

  it("an edit staged in one scope does not leak into the other", () => {
    /** Nothing has been written yet, so the other scope really does still read
     * what is on disk. Once the save lands the host re-resolves both. */
    const resolved = resolveConfigRow(
      boolRow(),
      "global",
      edits("project", "notifications.on_finish", { kind: "clear" }),
    );
    expect(resolved.origin).toBe("set_here");
    expect(resolved.staged).toBe(false);
  });

  it("warns about a routable bind address and stays quiet about loopback", () => {
    const loopback = resolveConfigRow(textRow("127.0.0.1"), "project", NO_CONFIG_EDITS);
    expect(loopback.warning).toBeNull();
    const routable = resolveConfigRow(textRow("0.0.0.0"), "project", NO_CONFIG_EDITS);
    expect(routable.warning).toBe(ROUTABLE_BIND_WARNING);
    /** The caution follows the *staged* value, so it appears before the save
     * rather than after it. */
    const staged = resolveConfigRow(
      textRow("127.0.0.1"),
      "project",
      edits("project", "web.bind", { kind: "set", value: "192.168.2.14" }),
    );
    expect(staged.warning).toBe(ROUTABLE_BIND_WARNING);
  });
});

describe("nextConfigValue", () => {
  it("flips a toggle", () => {
    const resolved = resolveConfigRow(boolRow(), "project", NO_CONFIG_EDITS);
    expect(nextConfigValue(resolved)).toBe(false);
  });

  it("advances a choice through the host's options and wraps", () => {
    const row = choiceRow();
    let resolved = resolveConfigRow(row, "project", NO_CONFIG_EDITS);
    expect(nextConfigValue(resolved)).toBe("normal");
    resolved = resolveConfigRow(
      row,
      "project",
      edits("project", row.key, { kind: "set", value: "bright" }),
    );
    expect(nextConfigValue(resolved)).toBe("off");
  });

  it("sets nothing on a text field — `Space` opens its editor instead", () => {
    const resolved = resolveConfigRow(textRow("127.0.0.1"), "project", NO_CONFIG_EDITS);
    expect(nextConfigValue(resolved)).toBeNull();
  });

  it("sets nothing for a choice the host sent no options for", () => {
    /** A host that offers a choice with an empty option list — no agents
     * configured, say — is offering nothing to cycle to, and the browser does
     * not invent one. */
    const resolved = resolveConfigRow(
      { ...choiceRow(), choices: [] },
      "project",
      NO_CONFIG_EDITS,
    );
    expect(nextConfigValue(resolved)).toBeNull();
  });
});

describe("stagedChanges", () => {
  const doc: ConfigDoc = {
    projectName: "flightdeck",
    globalPath: "/home/u/.flightdeck/config.toml",
    projectPath: "/repo/.flightdeck/config.toml",
    rows: { global: [boolRow(), choiceRow()], project: [boolRow(), choiceRow()] },
  };

  it("carries each edit's own scope, so one save can write both files", () => {
    const staged: ConfigEdits = {
      global: { "ui.mode_border": { kind: "set", value: "bright" } },
      project: { "notifications.on_finish": { kind: "clear" } },
    };
    expect(stagedChanges(doc, staged)).toEqual([
      { scope: "global", key: "ui.mode_border", value: "bright" },
      { scope: "project", key: "notifications.on_finish" },
    ]);
    expect(stagedCount(staged)).toBe(2);
  });

  it("is empty when nothing is staged", () => {
    expect(stagedChanges(doc, NO_CONFIG_EDITS)).toEqual([]);
    expect(stagedCount(NO_CONFIG_EDITS)).toBe(0);
  });
});

describe("isLoopbackAddress", () => {
  it("knows the three spellings and treats everything else as routable", () => {
    expect(isLoopbackAddress("127.0.0.1")).toBe(true);
    expect(isLoopbackAddress(" ::1 ")).toBe(true);
    expect(isLoopbackAddress("LOCALHOST")).toBe(true);
    expect(isLoopbackAddress("0.0.0.0")).toBe(false);
    expect(isLoopbackAddress("192.168.2.14")).toBe(false);
  });
});
