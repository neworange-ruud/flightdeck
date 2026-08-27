import { describe, expect, it } from "vitest";
import {
  CONFIG_FIELDS,
  isLoopbackAddress,
  resolveConfigRow,
  selectableConfigFields,
} from "./config";
import type { ConfigEdit, ConfigField } from "./config";

/**
 * `resolveConfigRow`'s origin attribution, in isolation from the DOM — see
 * `ui/configManager.test.ts` for the rendered rows. Pure logic only: no
 * store, no reducer, no jsdom.
 */

const NO_EDITS: Readonly<Record<string, ConfigEdit>> = {};

function field(key: string): ConfigField {
  const found = CONFIG_FIELDS.find((f) => f.key === key);
  if (found === undefined) {
    throw new Error(`no curated field ${key}`);
  }
  return found;
}

describe("resolveConfigRow", () => {
  it("set here: a project override wins in Project scope", () => {
    const resolved = resolveConfigRow(field("ui.agent_tab_position"), "project", NO_EDITS);
    expect(resolved.origin).toBe("set_here");
    expect(resolved.value).toBe("right");
  });

  it("global only: no project override, the global layer is what is in effect", () => {
    const resolved = resolveConfigRow(field("notifications.enabled"), "project", NO_EDITS);
    expect(resolved.origin).toBe("global");
    expect(resolved.value).toBe(true);
  });

  it("default only: neither layer has an explicit value", () => {
    const resolved = resolveConfigRow(field("notifications.sound"), "project", NO_EDITS);
    expect(resolved.origin).toBe("default");
    expect(resolved.value).toBe(false);
  });

  it("set here shadows a global value underneath it, until cleared", () => {
    const shadowing = field("notifications.on_finished");
    const set = resolveConfigRow(shadowing, "project", NO_EDITS);
    expect(set.origin).toBe("set_here");
    expect(set.value).toBe(true);

    const cleared = resolveConfigRow(shadowing, "project", {
      [shadowing.key]: { kind: "clear" },
    });
    expect(cleared.origin).toBe("global");
    expect(cleared.value).toBe(true);
  });

  it("Global scope reads the global file's own explicit value as set here", () => {
    const resolved = resolveConfigRow(field("notifications.enabled"), "global", NO_EDITS);
    expect(resolved.origin).toBe("set_here");
    expect(resolved.value).toBe(true);
  });

  it("Global scope falls back to the default when the global file has nothing", () => {
    const resolved = resolveConfigRow(field("ui.agent_tab_position"), "global", NO_EDITS);
    expect(resolved.origin).toBe("default");
    expect(resolved.value).toBe("left");
  });

  it("a host-only field has no browser-editable value or origin", () => {
    const resolved = resolveConfigRow(
      field("ui.use_f2_to_leave_terminal_focus"),
      "project",
      NO_EDITS,
    );
    expect(resolved.value).toBeNull();
    expect(resolved.origin).toBeNull();
    expect(resolved.warning).toBeNull();
  });
});

describe("D5: the routable-bind warning", () => {
  it("warns when the resolved bind is not loopback", () => {
    const resolved = resolveConfigRow(field("web.bind"), "project", NO_EDITS);
    expect(resolved.value).toBe("0.0.0.0");
    expect(resolved.warning).not.toBeNull();
  });

  it("is silent once the value resolves to the loopback default", () => {
    const bind = field("web.bind");
    const resolved = resolveConfigRow(bind, "project", {
      [bind.key]: { kind: "clear" },
    });
    expect(resolved.value).toBe("127.0.0.1");
    expect(resolved.warning).toBeNull();
  });

  it("is silent in Global scope, where no override is in play", () => {
    const resolved = resolveConfigRow(field("web.bind"), "global", NO_EDITS);
    expect(resolved.value).toBe("127.0.0.1");
    expect(resolved.warning).toBeNull();
  });
});

describe("isLoopbackAddress", () => {
  it.each(["127.0.0.1", "::1", "localhost", "LOCALHOST"])(
    "%s is loopback",
    (address) => {
      expect(isLoopbackAddress(address)).toBe(true);
    },
  );

  it.each(["0.0.0.0", "192.168.1.20", "example.com"])(
    "%s is not loopback",
    (address) => {
      expect(isLoopbackAddress(address)).toBe(false);
    },
  );
});

describe("selectableConfigFields", () => {
  it("excludes host-only fields", () => {
    const keys = selectableConfigFields().map((f) => f.key);
    expect(keys).not.toContain("ui.use_f2_to_leave_terminal_focus");
    expect(keys.length).toBe(CONFIG_FIELDS.length - 1);
  });
});
