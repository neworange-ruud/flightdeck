/**
 * @vitest-environment jsdom
 *
 * The layout below 900px (`remote-control-eek.4`,
 * `specs/WEB_INTERFACE.md` §6.5 R17) — artboard 1h's one paragraph, made
 * assertable.
 *
 * ## What this file can and cannot prove, stated up front
 *
 * jsdom has no layout engine. It does not compute a box, it does not resolve
 * `display: contents`, and — the reason the whole narrow layout is driven by a
 * `data-width` attribute rather than a media query — it parses `@media` and
 * never evaluates it. So:
 *
 *   **Proved here.** That 1h's breakpoint is where 1h puts it; that every
 *   element the narrow stylesheet needs exists, with the class and the
 *   attribute its selector keys off; that the slide-over opens, closes and is
 *   `hidden` exactly when it should be; that the git bar and the status bar
 *   are one wrapper's children so the fold has something to fold; that no
 *   fact is dropped from either strip at either width; and that every overlay
 *   still opens and is still operable while the frame says `narrow`.
 *
 *   **Not proved here.** That anything is the right size, in the right place,
 *   or on screen at all. Nothing in jsdom can tell a `column-reverse` from a
 *   `column`, or notice that a panel overflows. `webui/e2e/narrow.spec.ts`
 *   proves those, at 768×1024, in a real browser — including the D4 assertion
 *   that matters most: that a grid wider than the viewport scrolls and is
 *   never resized to fit.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { fixtureSnapshot } from "../state/fixture";
import { createApp } from "./app";
import type { App } from "./app";
import type { GitStatusPanel } from "../state/model";
import type { AppState } from "../state/types";

interface Harness {
  readonly app: App;
  q: (selector: string) => HTMLElement;
  maybe: (selector: string) => HTMLElement | null;
  all: (selector: string) => readonly HTMLElement[];
  text: (selector: string) => string;
  state: () => AppState;
  key: (key: string, init?: KeyboardEventInit) => void;
  /** The same key, pressed with focus where a real browser actually leaves it. */
  bodyKey: (key: string, init?: KeyboardEventInit) => void;
  click: (element: Element) => void;
  /** The one impurity `main.ts` owns, driven by hand here. */
  measure: (pixels: number) => void;
}

/** A portrait tablet, which is the width the acceptance criterion names. */
const TABLET = 768;
/** A laptop: comfortably 1a. */
const DESKTOP = 1440;

/** SPECS §21's panel, as `infoOverlay.test.ts` builds it — the same shape the
 * host sends, so this file is asserting the real panel and not a stub. */
function gitStatusPanel(): GitStatusPanel {
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

function render(options: { readonly pixels?: number } = {}): Harness {
  const app = createApp({
    mount: (container, _geometry, terminalId) => {
      container.append(`[${terminalId}]`);
    },
  });
  document.body.append(app.el);

  app.store.dispatch({ type: "snapshot/received", snapshot: fixtureSnapshot() });
  app.store.dispatch({ type: "connection/changed", status: "connected" });
  app.store.dispatch({
    type: "viewport/measured",
    pixels: options.pixels ?? TABLET,
  });

  const q = (selector: string): HTMLElement => {
    const found = app.el.querySelector<HTMLElement>(selector);
    if (found === null) {
      throw new Error(`no element matched ${selector}`);
    }
    return found;
  };

  return {
    app,
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
    bodyKey: (key, init = {}) => {
      document.body.dispatchEvent(
        new KeyboardEvent("keydown", { key, bubbles: true, ...init }),
      );
    },
    click: (element) => {
      element.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    },
    measure: (pixels) => app.store.dispatch({ type: "viewport/measured", pixels }),
  };
}

beforeEach(() => {
  document.body.replaceChildren();
});

describe("the frame says which layout it is in", () => {
  it("carries the width as an attribute, so CSS needs no media query", () => {
    const h = render({ pixels: DESKTOP });
    /** `.fd-frame` *is* `app.el`, so it is read rather than queried. */
    expect(h.app.el.getAttribute("data-width")).toBe("wide");
    h.measure(TABLET);
    expect(h.app.el.getAttribute("data-width")).toBe("narrow");
  });

  it("flips exactly at 1h's boundary, not near it", () => {
    const h = render({ pixels: 900 });
    expect(h.app.el.getAttribute("data-width")).toBe("wide");
    h.measure(899);
    expect(h.app.el.getAttribute("data-width")).toBe("narrow");
  });
});

describe("the keyboard reaches the app from where focus actually is", () => {
  /**
   * `remote-control-eek.4`, R17 §5. Every other keyboard test in this
   * repository dispatches its event on `app.el`, which is why none of them
   * ever noticed that the handler could not receive one.
   *
   * A keydown is delivered to listeners on the **ancestors of the focused
   * element**. `document.body` is an ancestor of `.fd-frame`, not a
   * descendant — so while the handler lived on the frame, a key pressed with
   * focus on the body went nowhere. Measured in Chromium, `activeElement` is
   * `BODY` on a fresh load and returns to `BODY` every time a focused control
   * is removed from the DOM, which every control here is: the regions rebuild
   * their children on each render. The practical effect was that no app-level
   * key worked until the user clicked the terminal, and clicking any chrome
   * control took them away again.
   *
   * jsdom models bubbling correctly, so this is one of the few browser-shaped
   * facts it *can* prove — as long as the event is dispatched where the
   * browser would really put it.
   */
  it("delivers §5's one chord with focus on the body", () => {
    const h = render();
    expect(document.activeElement).toBe(document.body);
    h.bodyKey("g", { ctrlKey: true });
    expect(h.state().palette).not.toBeNull();
  });

  it("delivers the App-mode plain keys from the body too", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    /** 1h's `s`, 2e's `a` and R16's `?` — the three plain keys the app claims,
     * all of which are only reachable this way on a tablet, where there is no
     * terminal click to "arm" the keyboard first. */
    h.bodyKey("s");
    expect(h.state().sidebarOpen).toBe(true);
    h.bodyKey("s");
    h.bodyKey("a");
    expect(h.state().feedOpen).toBe(true);
    h.bodyKey("a");
    h.bodyKey("?");
    expect(h.state().readOnly?.kind).toBe("help");
  });

  it("stops answering once its frame is off the page", () => {
    const h = render();
    /** The listener is on the document now, so a frame that has been taken
     * out of the page has to decline — otherwise a torn-down app keeps
     * reducing keys into a store nobody is reading, which in `vitest` means
     * every app a file has ever rendered. */
    h.app.el.remove();
    h.bodyKey("g", { ctrlKey: true });
    expect(h.state().palette).toBeNull();
  });
});

describe("1h's slide-over sidebar", () => {
  it("is the same element, hidden until asked for", () => {
    const h = render();
    /** Not a second component and not a second list — 1a's sidebar, moved. */
    expect(h.q(".fd-sidebar").hidden).toBe(true);
    expect(h.all(".fd-sidebar").length).toBe(1);

    h.app.store.dispatch({ type: "sidebar/set", open: true });
    expect(h.q(".fd-sidebar").hidden).toBe(false);
    /** And it is still the whole sidebar: 1a's six rows, not a summary. */
    expect(h.all(".fd-session").length).toBeGreaterThan(1);
  });

  it("is never hidden at wide, where it is 1a's column", () => {
    const h = render({ pixels: DESKTOP });
    expect(h.q(".fd-sidebar").hidden).toBe(false);
  });

  it("opens on the project row's session chip, which names the session", () => {
    const h = render();
    const chip = h.q(".fd-sessionchip");
    /** 1h: *"invoked from a session chip in the project row"*. */
    expect(h.q(".fd-projects").contains(chip)).toBe(true);
    /** At this width the chip is the only place the current session is named,
     * because the sidebar that names it has slid away. */
    expect(h.text(".fd-sessionchip__name")).toBe("fix-login-redirect");
    expect(chip.getAttribute("aria-expanded")).toBe("false");

    h.click(chip);
    expect(h.state().sidebarOpen).toBe(true);
    expect(h.q(".fd-sessionchip").getAttribute("aria-expanded")).toBe("true");
  });

  it("opens on `s` in App mode, and closes on `s` or `Esc`", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "app" });

    h.key("s");
    expect(h.state().sidebarOpen).toBe(true);
    /** The key that opened it closes it, exactly as 2e's `a` toggles the feed. */
    h.key("s");
    expect(h.state().sidebarOpen).toBe(false);

    h.key("s");
    h.key("Escape");
    expect(h.state().sidebarOpen).toBe(false);
  });

  it("leaves `s` alone in Terminal mode, where it is a letter", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "terminal" });
    h.key("s");
    /** 2e's own reasoning for `a`, applied: in Terminal mode `s` is a letter
     * the agent is waiting for. */
    expect(h.state().sidebarOpen).toBe(false);
  });

  it("leaves `s` alone at wide, where there is nothing to toggle", () => {
    const h = render({ pixels: DESKTOP });
    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    h.key("s");
    expect(h.state().sidebarOpen).toBe(false);
    expect(h.q(".fd-sidebar").hidden).toBe(false);
  });

  it("closes on a click outside, and that click still focuses the terminal", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    h.key("s");
    expect(h.state().sidebarOpen).toBe(true);

    h.click(h.q(".fd-mount"));
    expect(h.state().sidebarOpen).toBe(false);
    /** One tap, one intention: dismissing the panel by tapping the terminal
     * also wakes the terminal. Two taps for one gesture is the bug. */
    expect(h.state().mode).toBe("terminal");
  });

  it("stays open when the click is inside it", () => {
    const h = render();
    h.app.store.dispatch({ type: "sidebar/set", open: true });
    h.click(h.q(".fd-sidebar__title"));
    expect(h.state().sidebarOpen).toBe(true);
  });

  it("closes behind a session you picked, as 2e's feed does on a jump", () => {
    const h = render();
    h.app.store.dispatch({ type: "sidebar/set", open: true });
    const rows = h.all(".fd-session__select");
    h.click(rows[2] as HTMLElement);
    expect(h.state().sidebarOpen).toBe(false);
    expect(h.state().selection?.sessionId).toBe("s-migrate-schema-v4");
  });

  it("offers a close button that only exists at this width", () => {
    const h = render();
    h.app.store.dispatch({ type: "sidebar/set", open: true });
    /** 2e's `a close`, in the sidebar's own key. */
    expect(h.text(".fd-sidebar__close")).toContain("s");
    expect(h.text(".fd-sidebar__close")).toContain("close");
    h.click(h.q(".fd-sidebar__close"));
    expect(h.state().sidebarOpen).toBe(false);
  });

  it("keeps 2e's posture: complementary, no aria-modal, no scrim", () => {
    const h = render();
    h.app.store.dispatch({ type: "sidebar/set", open: true });
    const aside = h.q(".fd-sidebar");
    /** The sidebar is an `<aside>` with a label, which is a complementary
     * landmark already — the same semantics 2e's feed asserts, and the same
     * absence: nothing here traps focus or takes the screen. */
    expect(aside.tagName).toBe("ASIDE");
    expect(aside.getAttribute("aria-label")).toBe("Agents");
    expect(aside.hasAttribute("aria-modal")).toBe(false);
    expect(aside.getAttribute("role")).toBeNull();
    /** Unlike the palette it swallows nothing: `Enter` still focuses the
     * terminal underneath, because the terminal underneath is still live. */
    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    h.key("Enter");
    expect(h.state().mode).toBe("terminal");
  });

  it("closes itself when the viewport goes wide underneath it", () => {
    const h = render();
    h.app.store.dispatch({ type: "sidebar/set", open: true });
    h.measure(DESKTOP);
    expect(h.state().sidebarOpen).toBe(false);
    expect(h.q(".fd-sidebar").hidden).toBe(false);
  });
});

describe("1h's fold: the git bar into the status bar", () => {
  it("puts both strips in one wrapper, at both widths", () => {
    const h = render();
    const footer = h.q(".fd-footer");
    /** The fold is this box and a CSS rule. Nothing re-parents on a resize, so
     * there is no state in which the git bar is briefly nowhere. */
    expect(footer.contains(h.q(".fd-gitbar"))).toBe(true);
    expect(footer.contains(h.q(".fd-statusbar"))).toBe(true);
    h.measure(DESKTOP);
    expect(h.q(".fd-footer").contains(h.q(".fd-gitbar"))).toBe(true);
  });

  it("keeps the DOM order git-then-status, so a reader hears one order", () => {
    const h = render();
    const kids = [...h.q(".fd-footer").children].map((el) => el.className);
    expect(kids).toEqual(["fd-gitbar", "fd-statusbar"]);
  });

  it("drops no git fact into the fold", () => {
    const h = render();
    const bar = h.text(".fd-gitbar");
    /** Every fact 1a's git bar carries is still carried. The fold saves a
     * border and a background, never a number. */
    for (const fact of [
      "flightdeck/fix-login-redirect",
      "+3",
      "~2",
      "-1",
      "↑3",
      "↓0",
      "base +4",
      "base: main",
      "(6 files)",
    ]) {
      expect(bar, fact).toContain(fact);
    }
  });

  it("keeps 2c's rule 1: the spacer is still immediately before the strip", () => {
    const h = render();
    const spacer = h.q(".fd-statusbar .fd-spacer");
    expect(spacer.nextElementSibling?.classList.contains("fd-conn")).toBe(true);
  });

  it("keeps 1h's hints, in a box of their own", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "terminal" });
    const hints = h.q(".fd-statusbar__hints");
    /** 1h: *"the status bar states both routes permanently — no discovery
     * required"*. They move to their own line; they are not thinned out. */
    expect(hints.textContent).toContain("Esc Esc app commands");
    expect(hints.textContent).toContain("click outside release keys");
    expect(hints.textContent).toContain("Ctrl-g command palette");
    /** And the box is a direct child of the bar, which is what lets it be
     * ordered onto a second line without touching anything else. */
    expect(hints.parentElement?.className).toBe("fd-statusbar");
  });

  it("puts 2c's sentence in the same box, in the states that have one", () => {
    const h = render();
    h.app.store.dispatch({ type: "connection/changed", status: "reconnecting" });
    expect(h.text(".fd-statusbar__hints")).toContain(
      "keystrokes are being held",
    );
  });
});

describe("D4 below 900px", () => {
  it("still names the host's grid, and now says what it does when it will not fit", () => {
    const h = render();
    const chip = h.text(".fd-geometry");
    expect(chip).toContain("120×34 · host owns geometry");
    /**
     * The narrow half of D4's own explanation. It is a statement of policy,
     * true whether or not this grid overflows this viewport — deliberately,
     * because deciding it by measurement would mean the browser measuring
     * itself, which is the first step back towards a `FitAddon`.
     */
    expect(chip).toContain("scroll, never scale");
  });

  it("says nothing about scrolling at wide, where the margins are the story", () => {
    const h = render({ pixels: DESKTOP });
    expect(h.text(".fd-geometry")).toBe("120×34 · host owns geometry");
  });

  it("mounts the host's grid verbatim, and remounts for nothing else", () => {
    const mounts: { cols: number; rows: number }[] = [];
    const app = createApp({
      mount: (_container, geometry) => {
        mounts.push({ cols: geometry.cols, rows: geometry.rows });
      },
    });
    document.body.append(app.el);
    app.store.dispatch({ type: "snapshot/received", snapshot: fixtureSnapshot() });
    app.store.dispatch({ type: "viewport/measured", pixels: 390 });
    app.store.dispatch({ type: "viewport/measured", pixels: DESKTOP });
    app.store.dispatch({ type: "viewport/measured", pixels: 390 });
    /**
     * The invariant a `FitAddon` would break, asserted at the one width where
     * somebody would be tempted to add one: crossing 1h's breakpoint three
     * times mounts the terminal **once**, at the host's numbers. The browser
     * does not renegotiate geometry, because it never had any (D4/R4).
     */
    expect(mounts).toEqual([{ cols: 120, rows: 34 }]);
  });
});

describe("every overlay is still reachable at this width", () => {
  it("opens the palette, the config manager and all three read-only panels", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    expect(h.state().palette).not.toBeNull();
    expect(h.maybe(".fd-palette__panel")).not.toBeNull();
    h.key("Escape");

    h.app.store.dispatch({ type: "config/open" });
    expect(h.maybe(".fd-config__panel")).not.toBeNull();
    h.key("Escape");

    h.app.store.dispatch({ type: "help/open" });
    expect(h.q(".fd-info__panel").getAttribute("data-kind")).toBe("help");
    /** R16's two halves both render; neither is dropped for want of room. */
    expect(h.text(".fd-info__body")).toContain("This browser");
    h.key("Escape");

    h.app.store.dispatch({ type: "about/open" });
    expect(h.q(".fd-info__panel").getAttribute("data-kind")).toBe("about");
    h.key("Escape");

    h.app.store.dispatch({
      type: "gitStatus/received",
      panel: gitStatusPanel(),
    });
    expect(h.q(".fd-info__panel").getAttribute("data-kind")).toBe("git_status");
    /** Every fact row is still a row — the narrow stylesheet stacks the label
     * over the value, it does not drop the label. */
    expect(h.all(".fd-info__fact").length).toBeGreaterThan(1);
    for (const fact of h.all(".fd-info__fact")) {
      expect(fact.querySelector(".fd-info__fact-label")).not.toBeNull();
      expect(fact.querySelector(".fd-info__fact-value")).not.toBeNull();
    }
  });

  it("keeps 1d's two columns as two columns", () => {
    const h = render();
    h.key("g", { ctrlKey: true });
    /** Stacked by CSS, still two in the DOM — so `Tab next column` still
     * means what 1d's footer says it means. */
    expect(h.all(".fd-palette__column")).toHaveLength(2);
    h.key("Tab");
    expect(h.state().palette?.column).toBe(1);
  });

  it("keeps 1f's four cells per row, restacked rather than dropped", () => {
    const h = render();
    h.app.store.dispatch({ type: "config/open" });
    const rows = h.all(".fd-config__row");
    expect(rows.length).toBeGreaterThan(1);
    for (const row of rows) {
      expect(row.querySelector(".fd-config__label")).not.toBeNull();
      expect(row.querySelector(".fd-config__value")).not.toBeNull();
      /** Including the origin tag, which is 1f's whole point. */
      expect(row.querySelector(".fd-config__origin")).not.toBeNull();
    }
  });

  it("keeps 2e's feed on its own edge, opposite the sidebar", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    h.key("a");
    expect(h.state().feedOpen).toBe(true);
    expect(h.q(".fd-feed").hidden).toBe(false);
    /** Both can be open at once, which is why they do not share an edge. */
    h.key("s");
    expect(h.state().sidebarOpen).toBe(true);
    expect(h.state().feedOpen).toBe(true);
  });
});
