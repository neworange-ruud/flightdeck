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
  atNameStep,
  branchFieldVisible,
  cancelArgs,
  confirmArgs,
  decidingKeys,
  dialogOriginLabel,
  dialogStatus,
  gateSatisfied,
  gatedKey,
  hasToggle,
  primaryKey,
  selectedChoice,
  visibleChoices,
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
        { key: "Tab", label: "Target: new branch" },
        { key: "Esc", label: "Cancel", cancels: true },
      ],
      confirmable: true,
    },
  };
}

function newAgentBaseWire(): WireDialogView {
  const wire = newAgentWire();
  return {
    ...wire,
    title:
      "New Agent Session Tab   (↑/↓ agent · Tab changes target)\nRuns on base branch 'main' in the project root — no worktree.",
    body: {
      ...wire.body,
      input: null,
      buttons: [
        { key: "Enter", label: "Create" },
        { key: "Tab", label: "Target: base (main)" },
        { key: "Esc", label: "Cancel", cancels: true },
      ],
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
        { key: "n", label: "Cancel", cancels: true },
      ],
      confirmable: true,
    },
  };
}

/**
 * Artboard 1g's destructive confirmation, as the host really sends it: one
 * gated button (`y`), one that is not (`n` — cancel), and the whole of step 2
 * in `confirm_gate`.
 */
function abandonWire(): WireDialogView {
  return {
    dialog_id: "dialog-9",
    kind: "confirm_abandon",
    title: "The worktree has uncommitted changes. Discard them and abandon it?",
    origin: { origin: "desktop" },
    body: {
      buttons: [
        { key: "y", label: "Abandon (force)" },
        { key: "n", label: "Cancel", cancels: true },
      ],
      confirmable: true,
      confirm_gate: {
        key: "y",
        expected: "fix-login-redirect",
        instruction:
          "This browser is remote. Type the session name to abandon the worktree on the host.",
      },
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
    expect(dialog.draft).toEqual({
      text: "",
      index: null,
      /** 1g's field starts empty and at step 1: a gate that pre-filled itself
       * would be a button with extra steps. */
      confirmName: "",
      step: 1,
    });
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

  it("reads branch-field visibility from the host's current target", () => {
    expect(branchFieldVisible(dialogOf_(opened(newAgentWire())))).toBe(true);
    expect(branchFieldVisible(dialogOf_(opened(newAgentBaseWire())))).toBe(false);
  });

  it("filters a host-marked branch list with the local text draft", () => {
    const original = newAgentWire();
    const wire: WireDialogView = {
      ...original,
      body: {
        ...original.body,
        list_filter: true,
        list: [
          { label: "feature/existing", selected: true },
          { label: "release/v2", selected: false },
        ],
      },
    };
    let state = opened(wire);
    for (const char of "REL") {
      state = reduce(state, { type: "dialog/type", char });
    }
    const dialog = dialogOf_(state);
    expect(visibleChoices(dialog).map((choice) => choice.label)).toEqual([
      "release/v2",
    ]);
    expect(confirmArgs(dialog, "Enter")).toEqual({
      dialog_id: "dialog-7",
      list_index: 0,
      text: "REL",
    });
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

  it("keeps text but not a radio index when Tab changes the list domain", () => {
    let state = opened(newAgentWire());
    state = reduce(state, { type: "dialog/move", delta: 2 });
    state = reduce(state, { type: "dialog/type", char: "x" });
    const next = newAgentWire();
    state = opened(
      {
        ...next,
        body: { ...next.body, list_filter: true },
      },
      state,
    );
    expect(dialogOf_(state).draft).toMatchObject({ text: "x", index: null });
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

  it("sends no text when the host's base target has no field", () => {
    const state = opened(newAgentBaseWire());
    expect(confirmArgs(dialogOf_(state), "Enter")).toEqual({
      dialog_id: "dialog-7",
      list_index: 0,
    });
  });

  it("omits `choice` for the primary action and names any other button", () => {
    const state = opened(unpairWire());
    const dialog = dialogOf_(state);
    expect(primaryKey(dialog)).toEqual({
      key: "y",
      label: "Unpair",
      cancels: false,
    });
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

describe("artboard 1g's second step (ll5.4, §6.5 R13)", () => {
  it("reads the whole gate off the wire and invents none of it", () => {
    const dialog = dialogOf(abandonWire());
    expect(dialog.confirmable).toBe(true);
    expect(dialog.refusal).toBeNull();
    expect(dialog.gate).toEqual({
      key: "y",
      expected: "fix-login-redirect",
      instruction:
        "This browser is remote. Type the session name to abandon the worktree on the host.",
    });
    /** Which button is gated is the host's answer, not a local list of
     * dangerous-sounding kinds (R7 as amended by ll5.12). */
    expect(gatedKey(dialog, "y")).toBe(true);
    expect(gatedKey(dialog, "n")).toBe(false);
  });

  it("leaves an ordinary dialog ungated, so nothing grows a second step", () => {
    const dialog = dialogOf(unpairWire());
    expect(dialog.gate).toBeNull();
    expect(gatedKey(dialog, "y")).toBe(false);
    expect(atNameStep(dialog)).toBe(false);
  });

  it("matches the name exactly — no trim, no case fold", () => {
    let state = opened(abandonWire());
    state = reduce(state, { type: "dialog/advance" });
    const type = (text: string) => {
      for (const char of text) {
        state = reduce(state, { type: "dialog/gateType", char });
      }
    };

    /** Each of these is a name the host does not have, and the host compares
     * the same two strings the same way — so accepting one here would enable a
     * button the host is about to refuse. */
    for (const wrong of [
      "fix-login-redi",
      "Fix-Login-Redirect",
      "FIX-LOGIN-REDIRECT",
      "fix-login-redirect ",
      " fix-login-redirect",
      "flightdeck/fix-login-redirect",
    ]) {
      let attempt = state;
      for (const char of wrong) {
        attempt = reduce(attempt, { type: "dialog/gateType", char });
      }
      expect(gateSatisfied(dialogOf_(attempt))).toBe(false);
    }

    type("fix-login-redirect");
    expect(gateSatisfied(dialogOf_(state))).toBe(true);

    /** And a backspace takes it straight back out of the satisfied state. */
    state = reduce(state, { type: "dialog/gateBackspace" });
    expect(gateSatisfied(dialogOf_(state))).toBe(false);
  });

  it("advances locally: step 1 commits to nothing", () => {
    let state = opened(abandonWire());
    expect(dialogOf_(state).draft.step).toBe(1);
    expect(atNameStep(dialogOf_(state))).toBe(false);

    state = reduce(state, { type: "dialog/advance" });
    expect(dialogOf_(state).draft.step).toBe(2);
    expect(atNameStep(dialogOf_(state))).toBe(true);
    /** Nothing was sent: `pending` is what a frame would have added, and only
     * `dialog/dispatched` adds to it. */
    expect(dialogOf_(state).pending).toEqual([]);
  });

  it("never advances a dialog the host did not gate", () => {
    let state = opened(unpairWire());
    state = reduce(state, { type: "dialog/advance" });
    expect(dialogOf_(state).draft.step).toBe(1);
    state = reduce(state, { type: "dialog/gateType", char: "x" });
    expect(dialogOf_(state).draft.confirmName).toBe("");
  });

  it("puts the typed name on the gated confirm, and on nothing else", () => {
    let state = opened(abandonWire());
    state = reduce(state, { type: "dialog/advance" });
    for (const char of "fix-login-redirect") {
      state = reduce(state, { type: "dialog/gateType", char });
    }
    const dialog = dialogOf_(state);

    expect(confirmArgs(dialog, "y")).toEqual({
      dialog_id: "dialog-9",
      confirm_name: "fix-login-redirect",
    });
    /** The ungated button carries no name: sending one where the host does not
     * check it would teach the wire to expect it everywhere. */
    expect(confirmArgs(dialog, "n")).toEqual({
      dialog_id: "dialog-9",
      choice: "n",
    });
    /** Cancelling is never gated — R8: a shared dialog a remote surface can see
     * but not dismiss would be worse than not sharing it. */
    expect(cancelArgs(dialog)).toEqual({ dialog_id: "dialog-9" });
  });

  it("sends the name it has, wrong or empty, rather than a name it invented", () => {
    /** The browser does not correct, pad or substitute: if a frame goes out at
     * all it carries exactly what was typed, and the host refuses it. */
    let state = opened(abandonWire());
    state = reduce(state, { type: "dialog/advance" });
    expect(confirmArgs(dialogOf_(state), "y")).toEqual({
      dialog_id: "dialog-9",
      confirm_name: "",
    });
  });

  it("keeps the typed name across a re-announcement of the same dialog", () => {
    /** A coalesced resync must not empty 1g's field mid-typing, for the same
     * reason it must not empty 1e's branch field. */
    let state = opened(abandonWire());
    state = reduce(state, { type: "dialog/advance" });
    for (const char of "fix-login") {
      state = reduce(state, { type: "dialog/gateType", char });
    }
    state = opened(abandonWire(), state);
    expect(dialogOf_(state).draft.confirmName).toBe("fix-login");
    expect(dialogOf_(state).draft.step).toBe(2);
  });

  it("drops the typed name when a different dialog replaces it", () => {
    /** A different question is a different answer: carrying a half-typed name
     * across would put it in front of something nobody read. */
    let state = opened(abandonWire());
    state = reduce(state, { type: "dialog/advance" });
    for (const char of "fix-login") {
      state = reduce(state, { type: "dialog/gateType", char });
    }
    state = opened(unpairWire(), state);
    expect(dialogOf_(state).draft.confirmName).toBe("");
    expect(dialogOf_(state).draft.step).toBe(1);
  });
});
