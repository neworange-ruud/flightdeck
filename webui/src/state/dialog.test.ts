/**
 * D13's dialog as pure logic and as a reduction: what the origin line says, what
 * the draft does, and what one `dialog_confirm` frame carries.
 *
 * The rule every test here is defending: **the browser never decides that a
 * dialog is open, closed, or answered.** That is the host's, and a browser that
 * shortcut any of the three would be the second source of truth D13's "no new
 * state" rules out.
 */
import { describe, expect, it } from "vitest";
import {
  branchFieldVisible,
  cancelArgs,
  confirmArgs,
  decidingKeys,
  dialogOriginLabel,
  dialogStatus,
  hasToggle,
  primaryKey,
  selectedChoice,
} from "./dialog";
import { fixtureSnapshot } from "./fixture";
import { reduce } from "./reducer";
import { createInitialState } from "./types";
import type { AppState, DialogState } from "./types";
import { dialogOf } from "../wire/adapt";
import type { WireDialogView } from "../wire/frames";

/** Artboard 1e's new-agent dialog, as the host really sends it. */
function newAgentWire(
  origin: WireDialogView["origin"] = {
    origin: "browser",
    label: "192.168.2.20",
  },
): WireDialogView {
  return {
    dialog_id: "dialog-7",
    kind: "new_agent",
    title: "New Agent Session Tab",
    origin,
    body: {
      input: "",
      list: [
        { label: "(•) Claude Code", selected: true },
        { label: "( ) OpenCode", selected: false },
        { label: "( ) Codex CLI", selected: false },
      ],
      buttons: [
        { key: "Enter", label: "Create" },
        { key: "Tab", label: "Run from base: off" },
        { key: "Esc", label: "Cancel" },
      ],
      confirmable: true,
    },
  };
}

/** A y/n confirmation the desktop opened — no list, no field. */
function unpairWire(): WireDialogView {
  return {
    dialog_id: "dialog-3",
    kind: "unpair_phone",
    title: "Unpair this phone? It loses access until you pair it again.",
    origin: { origin: "desktop" },
    body: {
      buttons: [
        { key: "y", label: "Unpair" },
        { key: "n", label: "Cancel" },
      ],
      confirmable: true,
    },
  };
}

function opened(wire: WireDialogView, state?: AppState): AppState {
  return reduce(state ?? createInitialState(), {
    type: "dialog/opened",
    dialog: dialogOf(wire),
  });
}

function dialogOf_(state: AppState): DialogState {
  const dialog = state.dialog;
  if (dialog === null) {
    throw new Error("expected a dialog");
  }
  return dialog;
}

describe("the origin label (D13)", () => {
  it("names the browser that opened it, verbatim", () => {
    expect(
      dialogOriginLabel({ kind: "browser", label: "192.168.2.20" }),
    ).toBe("opened from browser · 192.168.2.20");
  });

  it("says so when the desktop opened it", () => {
    expect(dialogOriginLabel({ kind: "desktop" })).toBe(
      "opened on the desktop",
    );
  });

  it("carries a desktop origin with no label to invent", () => {
    const dialog = dialogOf(unpairWire());
    expect(dialog.origin).toEqual({ kind: "desktop" });
  });
});

describe("the wire → model mapping", () => {
  it("reads 1e's form off the body", () => {
    const dialog = dialogOf(newAgentWire());
    expect(dialog.kind).toBe("new_agent");
    expect(dialog.input).toBe("");
    expect(dialog.list).toHaveLength(3);
    expect(dialog.buttons.map((b) => b.key)).toEqual(["Enter", "Tab", "Esc"]);
    expect(dialog.confirmable).toBe(true);
  });

  it("starts with an empty draft and no local selection", () => {
    const dialog = dialogOf(newAgentWire());
    expect(dialog.draft).toEqual({ text: "", index: null, toggled: false });
    /** `index: null` means the host's own highlight stands, which for 1e is the
     * configured default agent — not a guess the browser made. */
    expect(selectedChoice(dialog)).toBe(0);
  });

  it("treats an absent `confirmable` as not confirmable", () => {
    /** Guessing `true` would send a confirm the host refuses, which is worse
     * than a disabled button: the user learns nothing until the round trip. */
    const dialog = dialogOf({
      dialog_id: "d",
      kind: "mystery",
      title: "?",
      origin: { origin: "desktop" },
    });
    expect(dialog.confirmable).toBe(false);
    expect(dialog.buttons).toEqual([]);
  });

  it("keeps the host's refusal sentence for a dialog it will not confirm", () => {
    const dialog = dialogOf({
      dialog_id: "d",
      kind: "confirm_abandon",
      title: "Abandon this worktree?",
      origin: { origin: "desktop" },
      body: {
        buttons: [{ key: "y", label: "Abandon" }],
        confirmable: false,
        refusal: "Abandoning a worktree discards work…",
      },
    });
    expect(dialog.confirmable).toBe(false);
    expect(dialog.refusal).toBe("Abandoning a worktree discards work…");
  });
});

describe("the dialog is the host's, not the browser's", () => {
  it("opens only because the host said so", () => {
    const state = opened(newAgentWire());
    expect(dialogOf_(state).id).toBe("dialog-7");
  });

  it("closes on a DialogClosed for the dialog that is open", () => {
    const state = reduce(opened(newAgentWire()), {
      type: "dialog/closed",
      dialogId: "dialog-7",
      outcome: "confirmed",
    });
    expect(state.dialog).toBeNull();
  });

  it("ignores a DialogClosed for a dialog that is not the open one", () => {
    /** A late close for a dialog the host already replaced must not take the
     * live one down with it. */
    const state = reduce(opened(newAgentWire()), {
      type: "dialog/closed",
      dialogId: "dialog-1",
      outcome: "superseded",
    });
    expect(dialogOf_(state).id).toBe("dialog-7");
  });

  it("does not close on an applied Ack — the host closes it", () => {
    let state = opened(newAgentWire());
    state = reduce(state, { type: "dialog/dispatched", seq: 4, act: "confirm" });
    state = reduce(state, {
      type: "command/result",
      seq: 4,
      outcome: "applied",
      detail: "Creating worktree for flightdeck/x…",
    });
    expect(state.dialog).not.toBeNull();
    expect(dialogOf_(state).lastOutcome).toEqual({
      outcome: "applied",
      detail: "Creating worktree for flightdeck/x…",
    });
  });

  it("a superseded close is reported as replaced, never as an answer", () => {
    /** `superseded` is the host saying nobody decided. Flattening it into a
     * cancel would claim somebody answered a question nobody answered. */
    let state = opened(newAgentWire());
    state = reduce(state, {
      type: "dialog/closed",
      dialogId: "dialog-7",
      outcome: "superseded",
    });
    expect(state.dialog).toBeNull();
    /** A replacement arrives as its own `dialog/opened`, with its own id and a
     * fresh draft. */
    state = opened(unpairWire(), state);
    expect(dialogOf_(state).id).toBe("dialog-3");
    expect(dialogOf_(state).draft.text).toBe("");
  });
});

describe("1e's local draft", () => {
  it("types into the branch field and takes characters back", () => {
    let state = opened(newAgentWire());
    for (const char of "refactor") {
      state = reduce(state, { type: "dialog/type", char });
    }
    state = reduce(state, { type: "dialog/backspace" });
    expect(dialogOf_(state).draft.text).toBe("refacto");
  });

  it("refuses to type into a dialog with no field", () => {
    let state = opened(unpairWire());
    state = reduce(state, { type: "dialog/type", char: "x" });
    expect(dialogOf_(state).draft.text).toBe("");
  });

  it("moves the agent radio, clamped rather than wrapped", () => {
    let state = opened(newAgentWire());
    state = reduce(state, { type: "dialog/move", delta: 1 });
    state = reduce(state, { type: "dialog/move", delta: 1 });
    expect(selectedChoice(dialogOf_(state))).toBe(2);
    state = reduce(state, { type: "dialog/move", delta: 1 });
    expect(selectedChoice(dialogOf_(state))).toBe(2);
    for (let i = 0; i < 5; i += 1) {
      state = reduce(state, { type: "dialog/move", delta: -1 });
    }
    expect(selectedChoice(dialogOf_(state))).toBe(0);
  });

  it("hides the branch field when run-from-base is on (1e, right)", () => {
    let state = opened(newAgentWire());
    expect(branchFieldVisible(dialogOf_(state))).toBe(true);
    state = reduce(state, { type: "dialog/toggle" });
    expect(dialogOf_(state).draft.toggled).toBe(true);
    expect(branchFieldVisible(dialogOf_(state))).toBe(false);
  });

  it("survives a coalesced resync of the same dialog", () => {
    /** A snapshot arrives for all sorts of reasons; emptying the field the user
     * is halfway through typing would be the visible bug. */
    let state = opened(newAgentWire());
    state = reduce(state, { type: "dialog/type", char: "x" });
    state = reduce(state, {
      type: "snapshot/received",
      snapshot: { ...fixtureSnapshot(), dialog: dialogOf(newAgentWire()) },
    });
    expect(dialogOf_(state).draft.text).toBe("x");
  });

  it("is discarded when a different dialog replaces it", () => {
    let state = opened(newAgentWire());
    state = reduce(state, { type: "dialog/type", char: "x" });
    state = opened(unpairWire(), state);
    expect(dialogOf_(state).draft.text).toBe("");
  });

  it("goes away with the dialog when the host's snapshot carries none", () => {
    let state = opened(newAgentWire());
    state = reduce(state, {
      type: "snapshot/received",
      snapshot: fixtureSnapshot(),
    });
    expect(state.dialog).toBeNull();
  });
});

describe("the confirm frame", () => {
  it("carries 1e's whole form", () => {
    let state = opened(newAgentWire());
    state = reduce(state, { type: "dialog/move", delta: 2 });
    for (const char of "relay-fanout") {
      state = reduce(state, { type: "dialog/type", char });
    }
    expect(confirmArgs(dialogOf_(state), "Enter")).toEqual({
      dialog_id: "dialog-7",
      list_index: 2,
      text: "relay-fanout",
    });
  });

  it("sends the toggle and drops the text when run-from-base is on", () => {
    /** 1e: the branch field is gone, so there is nothing to name — sending text
     * the host would ignore would be describing a form the user cannot see. */
    let state = opened(newAgentWire());
    for (const char of "ignored") {
      state = reduce(state, { type: "dialog/type", char });
    }
    state = reduce(state, { type: "dialog/toggle" });
    expect(confirmArgs(dialogOf_(state), "Enter")).toEqual({
      dialog_id: "dialog-7",
      toggle: true,
      list_index: 0,
    });
  });

  it("omits `choice` for the primary action and names any other button", () => {
    const state = opened(unpairWire());
    const dialog = dialogOf_(state);
    expect(primaryKey(dialog)).toEqual({ key: "y", label: "Unpair" });
    expect(confirmArgs(dialog, "y")).toEqual({ dialog_id: "dialog-3" });
    expect(confirmArgs(dialog, "n")).toEqual({
      dialog_id: "dialog-3",
      choice: "n",
    });
  });

  it("cancels by naming the dialog and nothing else", () => {
    expect(cancelArgs(dialogOf_(opened(newAgentWire())))).toEqual({
      dialog_id: "dialog-7",
    });
  });
});

describe("the keys a browser may press", () => {
  it("offers the deciding buttons and never Esc or Tab", () => {
    /** `Esc` has its own frame and `Tab` toggles rather than decides; letting a
     * confirm carry either would report the wrong outcome to the other
     * surface. */
    expect(decidingKeys(dialogOf(newAgentWire())).map((b) => b.key)).toEqual([
      "Enter",
    ]);
    expect(hasToggle(dialogOf(newAgentWire()))).toBe(true);
    expect(hasToggle(dialogOf(unpairWire()))).toBe(false);
  });
});

describe("what the panel says about the answer it sent", () => {
  it("says nothing before one was sent", () => {
    expect(dialogStatus(dialogOf(newAgentWire()))).toBeNull();
  });

  it("waits for the host rather than claiming a result", () => {
    const state = reduce(opened(newAgentWire()), {
      type: "dialog/dispatched",
      seq: 9,
      act: "confirm",
    });
    expect(dialogStatus(dialogOf_(state))?.tone).toBe("pending");
  });

  it("shows the host's own sentence for a refusal", () => {
    let state = opened(newAgentWire());
    state = reduce(state, { type: "dialog/dispatched", seq: 9, act: "confirm" });
    state = reduce(state, {
      type: "command/result",
      seq: 9,
      outcome: "rejected",
      detail: "The dialog is still open — it needs something it did not get.",
    });
    expect(dialogStatus(dialogOf_(state))).toEqual({
      tone: "rejected",
      text: "The dialog is still open — it needs something it did not get.",
    });
  });

  it("reports an observer's refusal as read-only, not as broken (D14)", () => {
    let state = opened(newAgentWire());
    state = reduce(state, { type: "dialog/dispatched", seq: 9, act: "confirm" });
    state = reduce(state, {
      type: "command/result",
      seq: 9,
      outcome: "read_only",
      detail: "this tab is watching read-only; take over to drive",
    });
    expect(dialogStatus(dialogOf_(state))?.tone).toBe("read_only");
  });

  it("ignores a result for a seq it never sent", () => {
    const state = reduce(opened(newAgentWire()), {
      type: "command/result",
      seq: 999,
      outcome: "applied",
    });
    expect(dialogOf_(state).lastOutcome).toBeNull();
  });
});
