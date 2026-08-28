/**
 * @vitest-environment jsdom
 *
 * The three read-only panels (`remote-control-ll5.8`,
 * `specs/WEB_INTERFACE.md` §6.5 R16), rendered through `createApp` exactly as
 * `configManager.test.ts` renders 1f — real elements, real keyboard events, no
 * snapshot files.
 *
 * What these tests are actually defending is the honesty rule, in the two
 * places it can be broken quietly: a panel that renders a fact the host never
 * sent, and a panel that renders a *zero* where the host said nothing. So
 * several of them assert an absence, and each says which absence it is.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { BROWSER_KEYS, HOST_SECTION_TITLE } from "../state/help";
import { fixtureAbout, fixtureHelp, fixtureSnapshot } from "../state/fixture";
import { createApp } from "./app";
import type { App } from "./app";
import type { PaletteCommand } from "../state/commands";
import type { GitStatusPanel, Snapshot } from "../state/model";
import type { AppState } from "../state/types";

interface Harness {
  readonly app: App;
  readonly run: PaletteCommand[];
  q: (selector: string) => HTMLElement;
  maybe: (selector: string) => HTMLElement | null;
  all: (selector: string) => readonly HTMLElement[];
  text: (selector: string) => string;
  state: () => AppState;
  key: (key: string, init?: KeyboardEventInit) => void;
}

function render(snapshot: Snapshot = fixtureSnapshot()): Harness {
  const run: PaletteCommand[] = [];
  const app = createApp({
    mount: (container, _geometry, terminalId) => {
      container.append(`[${terminalId}]`);
    },
    onRunCommand: (command) => run.push(command),
  });
  document.body.append(app.el);

  app.store.dispatch({ type: "snapshot/received", snapshot });
  app.store.dispatch({ type: "connection/changed", status: "connected" });
  app.store.dispatch({ type: "mode/set", mode: "app" });

  const q = (selector: string): HTMLElement => {
    const found = app.el.querySelector<HTMLElement>(selector);
    if (found === null) {
      throw new Error(`no element matched ${selector}`);
    }
    return found;
  };

  return {
    app,
    run,
    q,
    maybe: (selector) => app.el.querySelector<HTMLElement>(selector),
    all: (selector) => [...app.el.querySelectorAll<HTMLElement>(selector)],
    text: (selector) => q(selector).textContent ?? "",
    state: () => app.store.getState(),
    key: (key, init = {}) => {
      app.el.dispatchEvent(
        new KeyboardEvent("keydown", { key, bubbles: true, ...init }),
      );
    },
  };
}

function type(h: Harness, text: string): void {
  for (const char of text) {
    h.key(char);
  }
}

/** SPECS §21's panel as the host would send it: pushed, dirty, drifted, with a
 * compare URL — the fullest one there is, so a test asserting a row is missing
 * has something to have removed it from. */
function fullPanel(): GitStatusPanel {
  return {
    seq: 7,
    sessionId: "s-fix-login-redirect",
    sessionName: "fix-login-redirect",
    branch: "flightdeck/fix-login-redirect",
    baseBranch: "main",
    baseDrift: 4,
    dirty: true,
    changedFiles: 6,
    upstream: {
      name: "origin/flightdeck/fix-login-redirect",
      ahead: 3,
      behind: 1,
    },
    worktreePath: "/Users/ruud/worktrees/fix-login-redirect",
    compareUrl:
      "https://github.com/newOrange/flightdeck/compare/main...flightdeck/fix-login-redirect",
  };
}

function facts(h: Harness): Record<string, string> {
  const out: Record<string, string> = {};
  for (const fact of h.all(".fd-info__fact")) {
    const label = fact.querySelector(".fd-info__fact-label")?.textContent ?? "";
    out[label] = fact.querySelector(".fd-info__fact-value")?.textContent ?? "";
  }
  return out;
}

beforeEach(() => {
  document.body.replaceChildren();
});

// ---------------------------------------------------------------------------
// Opening and closing
// ---------------------------------------------------------------------------

describe("opening and closing", () => {
  it("nothing is open to start with", () => {
    const h = render();
    expect(h.state().readOnly).toBeNull();
    expect(h.q(".fd-info").hidden).toBe(true);
  });

  it("? opens help in App mode, and closes it again", () => {
    const h = render();
    h.key("?");
    expect(h.state().readOnly?.kind).toBe("help");
    expect(h.q(".fd-info").hidden).toBe(false);

    h.key("?");
    expect(h.state().readOnly).toBeNull();
    expect(h.q(".fd-info").hidden).toBe(true);
  });

  /**
   * §5 keeps a single `Esc` passing through to the agent and gives the app one
   * chord; the corollary for a *letter* is 2e's, and it is why `a` opens the
   * feed only in App mode. `?` follows the same rule: in Terminal mode it is a
   * character the agent is waiting for.
   */
  it("? is not claimed in Terminal mode — the agent is waiting for it", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "terminal" });
    h.key("?");
    expect(h.state().readOnly).toBeNull();
  });

  it("Esc closes whichever panel is up", () => {
    const h = render();
    h.app.store.dispatch({ type: "about/open" });
    expect(h.state().readOnly?.kind).toBe("about");
    h.key("Escape");
    expect(h.state().readOnly).toBeNull();
  });

  it("the panel's own close button closes it, and shows its key", () => {
    const h = render();
    h.app.store.dispatch({ type: "help/open" });
    const close = h.q(".fd-info__close");
    /** 1g: "every button shows its key". */
    expect(close.querySelector(".fd-key")?.textContent).toBe("Esc");
    close.click();
    expect(h.state().readOnly).toBeNull();
  });

  it("clicking outside the panel closes it", () => {
    const h = render();
    h.app.store.dispatch({ type: "help/open" });
    h.q(".fd-gitbar").dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(h.state().readOnly).toBeNull();
  });

  it("clicking inside the panel does not", () => {
    const h = render();
    h.app.store.dispatch({ type: "help/open" });
    h.q(".fd-info__body").dispatchEvent(
      new MouseEvent("click", { bubbles: true }),
    );
    expect(h.state().readOnly?.kind).toBe("help");
  });

  /**
   * The panel takes the keyboard while it is up, as the palette and the
   * configuration manager already do. `a` opening the activity feed *behind* a
   * scrim the reader cannot click through would be the app doing something
   * else while they are reading.
   */
  it("swallows the frame's other shortcuts while it is open", () => {
    const h = render();
    h.app.store.dispatch({ type: "help/open" });
    h.key("a");
    expect(h.state().feedOpen).toBe(false);
    expect(h.state().readOnly?.kind).toBe("help");
  });

  /** 2b's rule, for 2b's reason: the panel has a link and a close button, and
   * a keyboard-only reader has to be able to reach them. */
  it("never claims Tab", () => {
    const h = render();
    h.app.store.dispatch({ type: "about/open" });
    const event = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
    h.app.el.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(false);
  });

  /** One field holds one overlay, so "two panels at once" is not a state. */
  it("opening one panel replaces the other", () => {
    const h = render();
    h.app.store.dispatch({ type: "help/open" });
    h.app.store.dispatch({ type: "about/open" });
    expect(h.state().readOnly?.kind).toBe("about");
    expect(h.all(".fd-info__panel")).toHaveLength(1);
  });

  /**
   * R8's ruling made visible: these are not D13's dialogs. A dialog carries
   * `aria-modal` semantics and a decision; these carry neither, and the frame
   * behind them stays readable.
   */
  it("is a panel, not a modal question", () => {
    const h = render();
    h.app.store.dispatch({ type: "help/open" });
    expect(h.q(".fd-info").getAttribute("aria-modal")).toBeNull();
    expect(h.maybe(".fd-info .fd-dialog__action")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// The palette rows
// ---------------------------------------------------------------------------

describe("the palette rows", () => {
  it("Show Help opens the browser's own panel and sends nothing", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    type(h, "show help");
    const row = h.q(".fd-palette__row");
    expect(row.textContent).toContain("Show Help");
    row.click();

    expect(h.state().readOnly?.kind).toBe("help");
    expect(h.state().palette).toBeNull();
    /** The whole point of intercepting it: forwarding would open a panel on
     * the desktop that the person who asked cannot read. */
    expect(h.run).toHaveLength(0);
  });

  it("About FlightDeck opens the browser's own panel and sends nothing", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    type(h, "about");
    const row = h.q(".fd-palette__row");
    expect(row.textContent).toContain("About FlightDeck");
    row.click();

    expect(h.state().readOnly?.kind).toBe("about");
    expect(h.run).toHaveLength(0);
  });

  /**
   * The one that *is* sent. Its facts are a fresh `git` read the snapshot
   * cannot hold, so the row goes to the host like any other command — and the
   * panel does **not** open on the request. Opening it optimistically would
   * mean rendering a panel with nothing in it, or with numbers this tab made
   * up, which is exactly what R16 forbids.
   */
  it("Show Git Status is sent to the host, and opens nothing on its own", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    type(h, "git status");
    const row = h.q(".fd-palette__row");
    expect(row.textContent).toContain("Show Git Status");
    row.click();

    expect(h.run.map((c) => c.run.name)).toEqual(["show_git_status"]);
    expect(h.state().readOnly).toBeNull();
  });

  it("the panel opens when the host's answer arrives", () => {
    const h = render();
    h.app.store.dispatch({ type: "gitStatus/received", panel: fullPanel() });
    expect(h.state().readOnly?.kind).toBe("git_status");
    expect(h.text(".fd-info__title")).toBe("Git Status");
    /** SPECS §21 is about the *active Agent Tab*, and the host names it. */
    expect(h.text(".fd-info__subtitle")).toBe("fix-login-redirect");
  });
});

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

describe("the help panel", () => {
  it("renders the host's sections and rows verbatim", () => {
    const h = render();
    h.app.store.dispatch({ type: "help/open" });
    const body = h.text(".fd-info__body");

    for (const section of fixtureHelp().sections) {
      expect(body).toContain(section.title);
      for (const row of section.rows) {
        expect(body).toContain(row.keys);
        expect(body).toContain(row.description);
      }
    }
  });

  /**
   * The drift this whole design exists to prevent: the browser must render
   * *what the host sent*, not a list of its own. The fixture's help is
   * deliberately shorter than the real one, so a component carrying a private
   * copy would render rows that are not in it.
   */
  it("has no keybinding list of its own for the host's keyboard", () => {
    const h = render();
    h.app.store.dispatch({ type: "help/open" });
    const body = h.text(".fd-info__body");
    /** In the real `help_doc`, not in the fixture. */
    expect(body).not.toContain("Restart primary agent");
    expect(body).not.toContain("Ctrl-t");
  });

  it("states this browser's own keys, labelled as this browser's", () => {
    const h = render();
    h.app.store.dispatch({ type: "help/open" });
    const body = h.text(".fd-info__body");
    for (const row of BROWSER_KEYS) {
      expect(body).toContain(row.keys);
      expect(body).toContain(row.description);
    }
  });

  /**
   * D16's badge on the host's group. Rendering thirty chords in a browser tab
   * without saying they act elsewhere would be the app's first outright lie:
   * `Ctrl-q` typed here goes to the agent, not to FlightDeck.
   */
  it("says the host's keys act on the host, with D16's badge", () => {
    const h = render();
    h.app.store.dispatch({ type: "help/open" });
    const heading = h
      .all(".fd-info__group-head")
      .find((g) => g.textContent?.includes(HOST_SECTION_TITLE));
    expect(heading).toBeDefined();
    expect(heading?.querySelector(".fd-badge-host")).not.toBeNull();
    expect(h.text(".fd-info__body")).toContain("not in this tab");
  });

  /** SPECS §32's note leads the host's half, in the host's own order. */
  it("renders the host's notes, and only when the host sent one", () => {
    const plain = render();
    plain.app.store.dispatch({ type: "help/open" });
    expect(plain.maybe(".fd-info__hostnote")).toBeNull();

    const isolated = render({
      ...fixtureSnapshot(),
      help: {
        ...fixtureHelp(),
        notes: [
          {
            title: "Isolated run (--isolated)",
            lines: ["Nothing is saved and nothing was continued."],
          },
        ],
      },
    });
    isolated.app.store.dispatch({ type: "help/open" });
    expect(isolated.text(".fd-info__hostnote")).toContain("Isolated run");
    expect(isolated.text(".fd-info__hostnote")).toContain("Nothing is saved");
  });

  /**
   * The absent state. A host that sent no help gets a sentence saying so —
   * never this tab's guess at what the host binds, which would document a
   * FlightDeck it is not attached to.
   */
  it("says so when the host sent no keybindings, and invents none", () => {
    const h = render({ ...fixtureSnapshot(), help: null });
    h.app.store.dispatch({ type: "help/open" });
    expect(h.text(".fd-info__absent")).toContain("did not send its keybindings");
    /** The browser's own half is still there — it is this tab's to know. */
    expect(h.text(".fd-info__body")).toContain("Command palette");
    /** But nothing from the host's list is. */
    expect(h.text(".fd-info__body")).not.toContain("Quit / close app");
  });
});

// ---------------------------------------------------------------------------
// About
// ---------------------------------------------------------------------------

describe("the About panel", () => {
  it("reports the host's version and credits, not the tab's", () => {
    const h = render();
    h.app.store.dispatch({ type: "about/open" });
    const about = fixtureAbout();
    expect(h.text(".fd-info__about-product")).toBe(about.name);
    expect(h.text(".fd-info__about-version")).toBe(`v${about.version}`);
    expect(h.text(".fd-info__body")).toContain(about.tagline);
    for (const credit of about.credits) {
      expect(h.text(".fd-info__body")).toContain(credit.name);
      expect(h.text(".fd-info__body")).toContain(credit.role);
    }
    const link = h.q(".fd-info__about-url") as HTMLAnchorElement;
    expect(link.getAttribute("href")).toBe(about.url);
  });

  it("says nothing about a host that said nothing", () => {
    const h = render({ ...fixtureSnapshot(), about: null });
    h.app.store.dispatch({ type: "about/open" });
    expect(h.text(".fd-info__absent")).toContain("did not send its version");
    expect(h.maybe(".fd-info__about-version")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Git status (SPECS §21)
// ---------------------------------------------------------------------------

describe("the git status panel", () => {
  it("shows every fact SPECS §21 asks for, from the host", () => {
    const h = render();
    const panel = fullPanel();
    h.app.store.dispatch({ type: "gitStatus/received", panel });
    const rows = facts(h);

    expect(rows["branch"]).toContain(panel.branch);
    expect(rows["base branch"]).toContain(panel.baseBranch);
    expect(rows["base drift"]).toContain("4 commits ahead since creation");
    expect(rows["worktree"]).toContain("dirty");
    expect(rows["worktree"]).toContain("6 files uncommitted");
    expect(rows["upstream"]).toContain(panel.upstream?.name ?? "");
    expect(rows["ahead / behind"]).toContain("↑3 ↓1");
    expect(rows["worktree path"]).toContain(panel.worktreePath);
  });

  it("says clean, and none, rather than counting to zero", () => {
    const h = render();
    h.app.store.dispatch({
      type: "gitStatus/received",
      panel: { ...fullPanel(), dirty: false, changedFiles: 0, baseDrift: 0 },
    });
    const rows = facts(h);
    expect(rows["worktree"]).toBe("clean");
    expect(rows["base drift"]).toBe("none");
  });

  /**
   * The unknown, as the unknown. `WorktreeStatus` literally holds `0`/`0` for
   * a branch with no upstream — the host never looked — so the ahead/behind
   * row is **absent** rather than `↑0 ↓0`, and `no-upstream` sits in 2g's
   * lifted tier because deleting it would lose the fact that a push has never
   * happened.
   */
  it("renders no-upstream, and omits ahead/behind entirely", () => {
    const h = render();
    h.app.store.dispatch({
      type: "gitStatus/received",
      panel: { ...fullPanel(), upstream: null, compareUrl: null },
    });
    const rows = facts(h);
    expect(rows["upstream"]).toBe("no-upstream");
    expect(rows["ahead / behind"]).toBeUndefined();
    expect(h.text(".fd-info__body")).not.toContain("↑0");
    /** 2g: a fact cannot be `--fd-text-decor`. */
    const value = h
      .all(".fd-info__fact")
      .find((f) =>
        f.querySelector(".fd-info__fact-label")?.textContent === "upstream",
      )
      ?.querySelector(".fd-tone-quiet");
    expect(value).not.toBeNull();
  });

  it("offers the compare URL as a real link when the host sent one", () => {
    const h = render();
    const panel = fullPanel();
    h.app.store.dispatch({ type: "gitStatus/received", panel });
    const link = h.q(".fd-info__link") as HTMLAnchorElement;
    expect(link.getAttribute("href")).toBe(panel.compareUrl);
    expect(link.getAttribute("target")).toBe("_blank");
    expect(link.getAttribute("rel")).toContain("noopener");
  });

  /**
   * SPECS §5: FlightDeck must not create GitHub PRs, and §14 gives the compare
   * URL as the whole of what it does instead. The panel has to read that way —
   * a caption saying "open a PR" would be the browser claiming an action the
   * host will never take.
   */
  it("never presents the compare URL as FlightDeck opening a pull request", () => {
    const h = render();
    h.app.store.dispatch({ type: "gitStatus/received", panel: fullPanel() });
    const body = h.text(".fd-info__body");
    expect(body).toContain("FlightDeck never creates the pull request itself");
    expect(body).not.toMatch(/open (a )?pull request/i);
    expect(body).not.toMatch(/create (a )?PR/i);
  });

  /** Absent, not an empty link and not a URL assembled here from the branch
   * name — that would invite the reader to a page that may not exist. */
  it("has no compare row at all when the host sent no URL", () => {
    const h = render();
    h.app.store.dispatch({
      type: "gitStatus/received",
      panel: { ...fullPanel(), compareUrl: null },
    });
    expect(facts(h)["compare"]).toBeUndefined();
    expect(h.maybe(".fd-info__link")).toBeNull();
    expect(h.text(".fd-info__body")).not.toContain("github.com");
  });

  /** D16: the path is on the machine running FlightDeck, not this one. */
  it("badges the worktree path as the host's", () => {
    const h = render();
    h.app.store.dispatch({ type: "gitStatus/received", panel: fullPanel() });
    const row = h
      .all(".fd-info__fact")
      .find(
        (f) =>
          f.querySelector(".fd-info__fact-label")?.textContent ===
          "worktree path",
      );
    expect(row?.querySelector(".fd-badge-host")).not.toBeNull();
  });

  /** Nothing is asked, so there is nothing to answer — R8, as markup. */
  it("offers no confirm and no cancel", () => {
    const h = render();
    h.app.store.dispatch({ type: "gitStatus/received", panel: fullPanel() });
    expect(h.all(".fd-info button")).toHaveLength(1);
    expect(h.text(".fd-info button")).toContain("close");
  });
});
