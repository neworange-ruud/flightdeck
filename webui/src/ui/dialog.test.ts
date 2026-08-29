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

/** 1e's two titles, worded by the host (`new_agent_title` in `src/lib.rs`) and
 * sent together, because the toggle they describe is a local draft here. */
const NEW_AGENT_TITLE_OFF =
  "New Agent Session Tab   (↑/↓ agent · type branch · Tab = run from base branch)";
const NEW_AGENT_TITLE_ON =
  "New Agent Session Tab   (↑/↓ agent · Tab toggles base)\nRuns on base branch 'main' in the project root — no worktree.";

/** 1e, as the host sends it when a *browser* asked for it. */
function newAgentFromBrowser(): WireDialogView {
  return {
    dialog_id: "dialog-7",
    kind: "new_agent",
    title: NEW_AGENT_TITLE_OFF,
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
        { key: "Esc", label: "Cancel", cancels: true },
      ],
      confirmable: true,
      /** §6.5 R19: both wordings, so the panel and its own button can never
       * disagree about which state the local draft is in. */
      toggle: {
        key: "Tab",
        on: false,
        title_off: NEW_AGENT_TITLE_OFF,
        label_off: "Run from base: off",
        title_on: NEW_AGENT_TITLE_ON,
        label_on: "Run from base: main",
      },
    },
  };
}

/** The same form as the host sends it once the **desktop** has pressed `Tab`:
 * the host's own title and button label are the toggled ones, and `on` says so.
 */
function newAgentToggledOnTheHost(): WireDialogView {
  const wire = newAgentFromBrowser();
  return {
    ...wire,
    title: NEW_AGENT_TITLE_ON,
    body: {
      ...wire.body,
      buttons: [
        { key: "Enter", label: "Create" },
        { key: "Tab", label: "Run from base: main" },
        { key: "Esc", label: "Cancel", cancels: true },
      ],
      confirmable: true,
      toggle: {
        key: "Tab",
        on: true,
        title_off: NEW_AGENT_TITLE_OFF,
        label_off: "Run from base: off",
        title_on: NEW_AGENT_TITLE_ON,
        label_on: "Run from base: main",
      },
    },
  };
}

/** The same form, opened at the desktop's keyboard. */
function newAgentFromDesktop(): WireDialogView {
  return { ...newAgentFromBrowser(), origin: { origin: "desktop" } };
}

/**
 * Artboard 1g's destructive confirmation: shared, cancellable, and confirmable
 * only through the typed-name step the host published with it.
 */
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

/** A plain y/n confirmation with no second step — SPECS §19's close-shell. */
function closeTerminalFromDesktop(): WireDialogView {
  return {
    dialog_id: "dialog-4",
    kind: "close_terminal",
    title: "Close shell 2?",
    origin: { origin: "desktop" },
    body: {
      buttons: [
        { key: "y", label: "Close" },
        { key: "n", label: "Cancel", cancels: true },
      ],
      confirmable: true,
    },
  };
}

/**
 * The one dialog this build still refuses to confirm from a browser: a gate
 * whose subject the host can no longer name, because the session it asked about
 * is gone. Cancelling stays available, as it does everywhere.
 */
function unresolvedGateFromDesktop(): WireDialogView {
  return {
    ...abandonFromDesktop(),
    body: {
      buttons: [
        { key: "y", label: "Abandon (force)" },
        { key: "n", label: "Cancel", cancels: true },
      ],
      confirmable: false,
      refusal:
        "The host can no longer name what this would destroy — the session it asked about is gone. Cancel this dialog and start again.",
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
    expect(h.text(".fd-dialog__title")).toBe(NEW_AGENT_TITLE_OFF);
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

  it("the title and the Tab button move with the draft, not with the host", () => {
    /** **§6.5 R19, the whole bug.** The toggle is a local draft (R8), so the
     * host's `run_on_base` has not moved: before this the panel badged itself
     * `no worktree` while its own button still read `Run from base: off`. Both
     * wordings now arrive together and the browser picks by its draft. */
    const h = render();
    h.open(newAgentFromBrowser());
    const tab = () =>
      h.all(".fd-dialog__action").find((b) => b.getAttribute("data-key") === "Tab");
    expect(h.text(".fd-dialog__title")).toBe(NEW_AGENT_TITLE_OFF);
    expect(tab()?.textContent).toContain("Run from base: off");
    expect(tab()?.getAttribute("data-on")).toBe("false");

    h.key("Tab");
    expect(h.text(".fd-dialog__title")).toBe(NEW_AGENT_TITLE_ON);
    expect(tab()?.textContent).toContain("Run from base: main");
    expect(tab()?.getAttribute("data-on")).toBe("true");
    /** And back, on the same host frame — nothing was refetched. */
    h.key("Tab");
    expect(h.text(".fd-dialog__title")).toBe(NEW_AGENT_TITLE_OFF);
    expect(tab()?.textContent).toContain("Run from base: off");
  });

  it("opens in the state the host is already in, and words it the same way", () => {
    /** A tab attaching to a form the desktop had already switched to
     * run-from-base: the draft starts on `toggle.on`, so the panel is not
     * showing the other state's words either. */
    const h = render();
    h.open(newAgentToggledOnTheHost());
    expect(h.q(".fd-dialog__panel").getAttribute("data-toggled")).toBe("true");
    expect(h.text(".fd-dialog__title")).toBe(NEW_AGENT_TITLE_ON);
    expect(h.text(".fd-dialog__kind")).toBe("no worktree");
    expect(
      h.all(".fd-dialog__action")
        .find((b) => b.getAttribute("data-key") === "Tab")?.textContent,
    ).toContain("Run from base: main");

    h.key("Tab");
    expect(h.text(".fd-dialog__title")).toBe(NEW_AGENT_TITLE_OFF);
    expect(h.text(".fd-dialog__field-label")).toBe("Branch");
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
    h.open(closeTerminalFromDesktop());
    h.key("y");
    expect(h.answers).toEqual(["y"]);
  });

  it("a key the dialog is not showing does nothing", () => {
    const h = render();
    h.open(closeTerminalFromDesktop());
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
    h.open(unresolvedGateFromDesktop());

    expect(h.q(".fd-dialog__refusal").hidden).toBe(false);
    expect(h.text(".fd-dialog__refusal")).toContain("no longer name");
    const abandon = h
      .all(".fd-dialog__action")
      .find((b) => b.getAttribute("data-key") === "y");
    expect(abandon?.hasAttribute("disabled")).toBe(true);
    abandon?.click();
    expect(h.answers).toEqual([]);
  });

  it("still offers Cancel — dismissing cannot destroy anything", () => {
    const h = render();
    h.open(unresolvedGateFromDesktop());
    const cancel = h
      .all(".fd-dialog__action")
      .find((b) => b.getAttribute("data-key") === "Esc");
    expect(cancel?.hasAttribute("disabled")).toBe(false);
    cancel?.click();
    expect(h.answers).toEqual([null]);
  });
});

/**
 * **Artboard 1g, end to end in the browser** (`remote-control-ll5.4`, §6.5 R13).
 *
 * Every test here is a refusal path: the ones that matter are the ones proving
 * no `dialog_confirm` frame left this tab. `h.answers` is that proof — it
 * records what `main.ts` would have sent, so an empty array is "nothing was
 * answered", not "we did not look".
 */
describe("the two-step destructive confirmation (1g)", () => {
  it("opens at step 1, with the consequences and the keyed buttons", () => {
    const h = render();
    h.open(abandonFromDesktop());
    expect(h.q(".fd-dialog__panel").getAttribute("data-step")).toBe("1");
    expect(h.text(".fd-dialog__kind")).toBe("step 1 of 2");
    expect(h.q(".fd-dialog__gate").hidden).toBe(true);
    /** The question is the host's own words, read before anything is decided. */
    expect(h.text(".fd-dialog__title")).toContain("abandon it?");
  });

  it("pressing the gated button opens step 2 and sends nothing", () => {
    const h = render();
    h.open(abandonFromDesktop());
    h.key("y");
    expect(h.answers).toEqual([]);
    expect(h.q(".fd-dialog__panel").getAttribute("data-step")).toBe("2");
    expect(h.text(".fd-dialog__kind")).toBe("step 2 of 2 — confirm");
    expect(h.q(".fd-dialog__gate").hidden).toBe(false);
    /** The host's sentence, and the name it is waiting for, both verbatim. */
    expect(h.text(".fd-dialog__gate-instruction")).toContain(
      "This browser is remote",
    );
    expect(h.text(".fd-dialog__gate-hint")).toBe("fix-login-redirect");
  });

  it("clicking the gated button does the same thing the key does", () => {
    const h = render();
    h.open(abandonFromDesktop());
    h.all(".fd-dialog__action")
      .find((b) => b.getAttribute("data-key") === "y")
      ?.click();
    expect(h.answers).toEqual([]);
    expect(h.q(".fd-dialog__panel").getAttribute("data-step")).toBe("2");
  });

  it("refuses to send a wrong or partial name", () => {
    const h = render();
    h.open(abandonFromDesktop());
    h.key("y");

    for (const char of "fix-login-redi") {
      h.key(char);
    }
    expect(h.text(".fd-dialog__typed")).toBe("fix-login-redi");
    const confirm = () =>
      h.all(".fd-dialog__action").find((b) => b.getAttribute("data-key") === "Enter");
    expect(confirm()?.hasAttribute("disabled")).toBe(true);

    /** Neither the key nor the click gets a frame out. */
    h.key("Enter");
    confirm()?.click();
    expect(h.answers).toEqual([]);

    /** And the panel has not moved on: the question is still on screen. */
    expect(h.q(".fd-dialog").hidden).toBe(false);
    expect(h.q(".fd-dialog__panel").getAttribute("data-step")).toBe("2");
  });

  it("refuses a name that differs only in case or whitespace", () => {
    const h = render();
    h.open(abandonFromDesktop());
    h.key("y");
    for (const char of "Fix-Login-Redirect") {
      h.key(char);
    }
    h.key("Enter");
    expect(h.answers).toEqual([]);

    /** Same for a trailing space, once the letters are right. */
    const h2 = render();
    h2.open(abandonFromDesktop());
    h2.key("y");
    for (const char of "fix-login-redirect ") {
      h2.key(char);
    }
    h2.key("Enter");
    expect(h2.answers).toEqual([]);
  });

  it("sends the gated key once the name matches exactly", () => {
    const h = render();
    h.open(abandonFromDesktop());
    h.key("y");
    for (const char of "fix-login-redirect") {
      h.key(char);
    }
    const confirm = h
      .all(".fd-dialog__action")
      .find((b) => b.getAttribute("data-key") === "Enter");
    expect(confirm?.hasAttribute("disabled")).toBe(false);
    /** 1g prints the host's own verb on step 2's button. */
    expect(confirm?.textContent).toContain("Abandon (force)");

    h.key("Enter");
    expect(h.answers).toEqual(["y"]);
    /** Still the host's to close: an answer is not an outcome. */
    expect(h.q(".fd-dialog").hidden).toBe(false);
  });

  it("a backspace takes the confirm away again", () => {
    const h = render();
    h.open(abandonFromDesktop());
    h.key("y");
    for (const char of "fix-login-redirect") {
      h.key(char);
    }
    h.key("Backspace");
    h.key("Enter");
    expect(h.answers).toEqual([]);
  });

  it("cancels from step 2 with no name typed at all", () => {
    /** R8's property, at the step where it is easiest to lose: dismissing a
     * confirmation cannot destroy anything, so it is never gated. */
    const h = render();
    h.open(abandonFromDesktop());
    h.key("y");
    h.key("f");
    const cancel = h
      .all(".fd-dialog__action")
      .find((b) => b.getAttribute("data-key") === "Esc");
    expect(cancel?.hasAttribute("disabled")).toBe(false);
    h.key("Escape");
    expect(h.answers).toEqual([null]);
  });

  it("leaves an ungated dialog answering on the first press", () => {
    /** The gate is the host's per-button answer, not a browser-side rule about
     * dialogs that look dangerous: a `y`/`n` confirmation with no gate still
     * decides on `y`. */
    const h = render();
    h.open(closeTerminalFromDesktop());
    expect(h.q(".fd-dialog__panel").getAttribute("data-step")).toBe("");
    h.key("y");
    expect(h.answers).toEqual(["y"]);
    expect(h.q(".fd-dialog__gate").hidden).toBe(true);
  });
});

/**
 * **§6.5 R19's other half.** 1g's step 1 draws two buttons — the verb and one
 * cancel — and the browser was drawing three, because the host's own `n Cancel`
 * arrived as a deciding key and this component appended its `Esc Cancel` on top.
 */
describe("one cancel, and the destructive verb beside it", () => {
  it("draws the verb and a single cancel at step 1", () => {
    const h = render();
    h.open(abandonFromDesktop());
    expect(h.all(".fd-dialog__action").map((b) => b.getAttribute("data-key")))
      .toEqual(["y", "Esc"]);
    /** The word is still the host's — read off the button it marked `cancels`. */
    expect(
      h.all(".fd-dialog__action")[1]?.textContent,
    ).toContain("Cancel");
  });

  it("styles the destructive verb apart from the cancel", () => {
    const h = render();
    h.open(abandonFromDesktop());
    const [verb, cancel] = h.all(".fd-dialog__action");
    /** `danger` is worn by the button the **host** gated, never by a browser's
     * guess about which dialogs look dangerous. */
    expect(verb?.className).toContain("fd-dialog__action--danger");
    expect(cancel?.className).toContain("fd-dialog__action--tertiary");
  });

  it("the host's own cancel key still cancels, through `dialog_cancel`", () => {
    /** It is no longer a button, but `n` is what the desktop prints and what a
     * reader of the two surfaces will press. It sends the cancel frame, which
     * is never gated — R8 — rather than a confirm carrying `n`. */
    const h = render();
    h.open(closeTerminalFromDesktop());
    expect(h.all(".fd-dialog__action").map((b) => b.getAttribute("data-key")))
      .toEqual(["y", "Esc"]);
    h.key("n");
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
