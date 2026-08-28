/**
 * @vitest-environment jsdom
 *
 * Artboard `1e — NEW AGENT DIALOG, BOTH STATES` rendered through `createApp`,
 * the same way `commandPalette.test.ts` renders 1d and `configManager.test.ts`
 * renders 1f — real elements, real keyboard events, no snapshot files.
 *
 * Three properties are load-bearing and each has a test below:
 *
 * 1. **The origin line is drawn.** D13's accepted cost is a modal you did not
 *    ask for; the label is the only thing that explains it.
 * 2. **Nothing local opens or closes the dialog.** `Esc` sends `dialog_cancel`
 *    and the panel stays until the host's `Delta::DialogClosed` arrives.
 * 3. **An observer's answer is the host's refusal, shown verbatim.**
 */
import { describe, expect, it } from "vitest";
import { fixtureSnapshot } from "../state/fixture";
import { createApp } from "./app";
import type { App } from "./app";
import type { AppState } from "../state/types";
import { dialogOf } from "../wire/adapt";
import type { WireDialogView } from "../wire/frames";

interface Harness {
  readonly app: App;
  /** Every answer reported through `onAnswerDialog`; `null` is a cancel. */
  readonly answers: (string | null)[];
  q: (selector: string) => HTMLElement;
  all: (selector: string) => readonly HTMLElement[];
  text: (selector: string) => string;
  state: () => AppState;
  key: (key: string, init?: KeyboardEventInit) => void;
  open: (wire: WireDialogView) => void;
}

function render(): Harness {
  const answers: (string | null)[] = [];
  const app = createApp({
    mount: (container, _geometry, terminalId) => {
      container.append(`[${terminalId}]`);
    },
    onAnswerDialog: (key) => answers.push(key),
  });
  document.body.append(app.el);
  app.store.dispatch({ type: "snapshot/received", snapshot: fixtureSnapshot() });
  app.store.dispatch({ type: "connection/changed", status: "connected" });

  const q = (selector: string): HTMLElement => {
    const found = app.el.querySelector<HTMLElement>(selector);
    if (found === null) {
      throw new Error(`no element matched ${selector}`);
    }
    return found;
  };

  return {
    app,
    answers,
    q,
    all: (selector) => [...app.el.querySelectorAll<HTMLElement>(selector)],
    text: (selector) => q(selector).textContent ?? "",
    state: () => app.store.getState(),
    key: (key, init = {}) => {
      app.el.dispatchEvent(
        new KeyboardEvent("keydown", { key, bubbles: true, ...init }),
      );
    },
    open: (wire) =>
      app.store.dispatch({ type: "dialog/opened", dialog: dialogOf(wire) }),
  };
}

/** 1e, as the host sends it when a *browser* asked for it. */
function newAgentFromBrowser(): WireDialogView {
  return {
    dialog_id: "dialog-7",
    kind: "new_agent",
    title: "New Agent Session Tab",
    origin: { origin: "browser", label: "192.168.2.20" },
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

/** The same form, opened at the desktop's keyboard. */
function newAgentFromDesktop(): WireDialogView {
  return { ...newAgentFromBrowser(), origin: { origin: "desktop" } };
}

/** A destructive confirmation: shared, cancellable, not confirmable from here. */
function abandonFromDesktop(): WireDialogView {
  return {
    dialog_id: "dialog-9",
    kind: "confirm_abandon",
    title:
      "The worktree has uncommitted changes. Discard them and abandon it?",
    origin: { origin: "desktop" },
    body: {
      buttons: [
        { key: "y", label: "Abandon (force)" },
        { key: "n", label: "Cancel" },
      ],
      confirmable: false,
      refusal:
        "Abandoning a worktree discards work, and from a browser that needs artboard 1g's two-step confirmation.",
    },
  };
}

describe("the dialog layer", () => {
  it("is not on screen until the host publishes a dialog", () => {
    const h = render();
    expect(h.q(".fd-dialog").hidden).toBe(true);
    expect(h.app.el.getAttribute("data-dialog")).toBe("false");
  });

  it("appears when the host publishes one, and marks the frame", () => {
    const h = render();
    h.open(newAgentFromBrowser());
    expect(h.q(".fd-dialog").hidden).toBe(false);
    expect(h.app.el.getAttribute("data-dialog")).toBe("true");
    expect(h.text(".fd-dialog__title")).toBe("New Agent Session Tab");
  });
});

describe("the origin line (D13)", () => {
  it("names the browser that opened it", () => {
    const h = render();
    h.open(newAgentFromBrowser());
    expect(h.text(".fd-dialog__origin")).toBe(
      "opened from browser · 192.168.2.20",
    );
    expect(h.q(".fd-dialog__origin").getAttribute("data-origin")).toBe(
      "browser",
    );
  });

  it("says so when the desktop opened it", () => {
    const h = render();
    h.open(newAgentFromDesktop());
    expect(h.text(".fd-dialog__origin")).toBe("opened on the desktop");
    expect(h.q(".fd-dialog__origin").getAttribute("data-origin")).toBe(
      "desktop",
    );
  });
});

describe("artboard 1e, left-hand state", () => {
  it("draws the agent radio, the branch field and the three keys", () => {
    const h = render();
    h.open(newAgentFromBrowser());

    const choices = h.all(".fd-dialog__choice");
    expect(choices).toHaveLength(3);
    expect(choices[0]?.getAttribute("data-selected")).toBe("true");
    expect(h.text(".fd-dialog__field-label")).toBe("Branch");
    expect(h.all(".fd-dialog__action").map((b) => b.getAttribute("data-key")))
      .toEqual(["Enter", "Tab", "Esc"]);
  });

  it("types the branch name into the field", () => {
    const h = render();
    h.open(newAgentFromBrowser());
    for (const char of "refactor-relay-fanout") {
      h.key(char);
    }
    expect(h.text(".fd-dialog__typed")).toBe("refactor-relay-fanout");
  });

  it("moves the radio with the arrow keys, and with a click", () => {
    const h = render();
    h.open(newAgentFromBrowser());
    h.key("ArrowDown");
    h.key("ArrowDown");
    expect(h.all(".fd-dialog__choice")[2]?.getAttribute("data-selected")).toBe(
      "true",
    );
    h.all(".fd-dialog__choice")[1]?.click();
    expect(h.all(".fd-dialog__choice")[1]?.getAttribute("data-selected")).toBe(
      "true",
    );
  });
});

describe("artboard 1e, right-hand state", () => {
  it("Tab hides the branch field and recolours the frame", () => {
    const h = render();
    h.open(newAgentFromBrowser());
    h.key("Tab");

    expect(h.q(".fd-dialog__panel").getAttribute("data-toggled")).toBe("true");
    /** 1e's own words, and a sentence rather than a disabled input: nobody
     * should have to wonder whether the text they typed is still in play. */
    expect(h.text(".fd-dialog__note")).toBe(
      "branch field hidden — there is nothing to name",
    );
    expect(h.all(".fd-dialog__field-label")).toHaveLength(0);
    expect(h.text(".fd-dialog__kind")).toBe("no worktree");
  });

  it("Tab is left to the browser on a dialog with no toggle", () => {
    /** Swallowing `Tab` on a y/n confirmation would leave a keyboard-only user
     * with a panel they can see and cannot reach. */
    const h = render();
    h.open(abandonFromDesktop());
    h.key("Tab");
    expect(h.q(".fd-dialog__panel").getAttribute("data-toggled")).toBe("false");
  });
});

describe("answering, and who is allowed to", () => {
  it("Enter reports the primary key and does NOT close the panel", () => {
    /** D13: the host closes the dialog on both surfaces. A local close would be
     * a second source of truth — and it would be wrong for the case that
     * matters, a form the host kept open because it needs something more. */
    const h = render();
    h.open(newAgentFromBrowser());
    h.key("Enter");
    expect(h.answers).toEqual(["Enter"]);
    expect(h.q(".fd-dialog").hidden).toBe(false);
  });

  it("Esc reports a cancel and does NOT close the panel either", () => {
    const h = render();
    h.open(newAgentFromBrowser());
    h.key("Escape");
    expect(h.answers).toEqual([null]);
    expect(h.q(".fd-dialog").hidden).toBe(false);
  });

  it("closes only when the host says it closed", () => {
    const h = render();
    h.open(newAgentFromBrowser());
    h.key("Enter");
    h.app.store.dispatch({
      type: "dialog/closed",
      dialogId: "dialog-7",
      outcome: "confirmed",
    });
    expect(h.q(".fd-dialog").hidden).toBe(true);
    expect(h.app.el.getAttribute("data-dialog")).toBe("false");
  });

  it("a keyed button fires that button, on a dialog with no text field", () => {
    const h = render();
    h.open({
      ...abandonFromDesktop(),
      body: { ...abandonFromDesktop().body, confirmable: true },
    });
    h.key("y");
    expect(h.answers).toEqual(["y"]);
  });

  it("a key the dialog is not showing does nothing", () => {
    const h = render();
    h.open({
      ...abandonFromDesktop(),
      body: { ...abandonFromDesktop().body, confirmable: true },
    });
    h.key("q");
    expect(h.answers).toEqual([]);
  });

  it("a click on a keyed button reports the same key as the keypress", () => {
    const h = render();
    h.open(newAgentFromBrowser());
    h.all(".fd-dialog__action")[0]?.click();
    expect(h.answers).toEqual(["Enter"]);
  });
});

describe("a dialog this build will not confirm from a browser", () => {
  it("shows the host's refusal and disables the confirm", () => {
    const h = render();
    h.open(abandonFromDesktop());

    expect(h.q(".fd-dialog__refusal").hidden).toBe(false);
    expect(h.text(".fd-dialog__refusal")).toContain("two-step confirmation");
    const abandon = h
      .all(".fd-dialog__action")
      .find((b) => b.getAttribute("data-key") === "y");
    expect(abandon?.hasAttribute("disabled")).toBe(true);
    abandon?.click();
    expect(h.answers).toEqual([]);
  });

  it("still offers Cancel — dismissing cannot destroy anything", () => {
    const h = render();
    h.open(abandonFromDesktop());
    const cancel = h
      .all(".fd-dialog__action")
      .find((b) => b.getAttribute("data-key") === "Esc");
    expect(cancel?.hasAttribute("disabled")).toBe(false);
    cancel?.click();
    expect(h.answers).toEqual([null]);
  });
});

describe("what the host said about the answer", () => {
  it("waits for the host rather than claiming a result", () => {
    const h = render();
    h.open(newAgentFromBrowser());
    h.app.store.dispatch({ type: "dialog/dispatched", seq: 4, act: "confirm" });
    expect(h.q(".fd-dialog__status").getAttribute("data-outcome")).toBe(
      "pending",
    );
  });

  it("shows an observer's refusal in the host's own words (D14)", () => {
    const h = render();
    h.open(newAgentFromBrowser());
    h.app.store.dispatch({ type: "dialog/dispatched", seq: 4, act: "confirm" });
    h.app.store.dispatch({
      type: "command/result",
      seq: 4,
      outcome: "read_only",
      detail: "this tab is watching read-only; take over to drive",
    });
    expect(h.q(".fd-dialog__status").getAttribute("data-outcome")).toBe(
      "read_only",
    );
    expect(h.text(".fd-dialog__status")).toBe(
      "this tab is watching read-only; take over to drive",
    );
    /** The dialog is still up: an observer sees the shared question, it simply
     * is not theirs to answer. */
    expect(h.q(".fd-dialog").hidden).toBe(false);
  });
});

describe("the dialog and the other overlays", () => {
  it("claims the keyboard ahead of the palette", () => {
    const h = render();
    h.app.store.dispatch({ type: "palette/open" });
    h.open(newAgentFromBrowser());
    /** A character goes into the dialog's field, not the palette's filter. */
    h.key("z");
    expect(h.state().palette?.filter).toBe("");
    expect(h.text(".fd-dialog__typed")).toBe("z");
  });

  it("does not release the terminal keys when clicked", () => {
    /** The same rule every overlay follows: an overlay is *over* the terminal,
     * not outside it, so a choice made in one must not change the mode. */
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "terminal" });
    h.open(newAgentFromBrowser());
    h.all(".fd-dialog__choice")[1]?.click();
    expect(h.state().mode).toBe("terminal");
  });
});
