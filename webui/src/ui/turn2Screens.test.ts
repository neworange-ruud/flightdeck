/**
 * @vitest-environment jsdom
 *
 * The turn-2 screens, rendered and asserted against artboards 2b/2c/2d/2e/2f.
 *
 * Same rules as `mainScreen.test.ts`: everything renderable is assertable, no
 * snapshot files and no screenshots — just the strings and the structure the
 * design specifies. xterm.js is never involved; `mount` is injected.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { fixtureActivity, fixtureSnapshot } from "../state/fixture";
import { ALL_CONNECTION_STATUSES } from "../state/connection";
import { createApp } from "./app";
import type { App } from "./app";
import type { AppAction, AppState, ConnectionStatus } from "../state/types";
import type { StripAction } from "../state/connection";

interface Harness {
  readonly app: App;
  readonly actions: StripAction[];
  readonly submitted: string[];
  readonly dispatched: AppAction[];
  q: (selector: string) => HTMLElement;
  maybe: (selector: string) => HTMLElement | null;
  all: (selector: string) => readonly HTMLElement[];
  text: (selector: string) => string;
  state: () => AppState;
  key: (key: string, init?: KeyboardEventInit) => void;
}

/** `attach: false` skips the snapshot, which is the pre-access situation. */
function render(options: { readonly attach?: boolean } = {}): Harness {
  const actions: StripAction[] = [];
  const submitted: string[] = [];
  const dispatched: AppAction[] = [];
  const app = createApp({
    mount: (container, _geometry, terminalId) => {
      container.append(`[${terminalId}]`);
    },
    onDispatch: (action) => dispatched.push(action),
    onStripAction: (action) => actions.push(action),
    onSubmitCode: (code) => submitted.push(code),
  });
  document.body.append(app.el);

  if (options.attach !== false) {
    app.store.dispatch({
      type: "snapshot/received",
      snapshot: fixtureSnapshot(),
    });
    app.store.dispatch({ type: "connection/changed", status: "connected" });
    app.store.dispatch({ type: "mode/set", mode: "terminal" });
  }
  app.store.dispatch({ type: "host/set", host: "192.168.2.14:7420" });

  const q = (selector: string): HTMLElement => {
    const found = app.el.querySelector<HTMLElement>(selector);
    if (found === null) {
      throw new Error(`no element matched ${selector}`);
    }
    return found;
  };

  return {
    app,
    actions,
    submitted,
    dispatched,
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

beforeEach(() => {
  document.body.replaceChildren();
});

/* ══ 2b — the four browser-side access screens ═══════════════════════════ */

describe("2b — access screens", () => {
  function needCode(
    h: Harness,
    screen: "code_entry" | "rejected" | "rate_limited",
    extra: { attemptsRemaining?: number; lockoutSeconds?: number } = {},
  ): void {
    h.app.store.dispatch({
      type: "access/required",
      screen,
      attemptsRemaining: extra.attemptsRemaining ?? null,
      lockoutSeconds: extra.lockoutSeconds ?? null,
    });
  }

  it("code entry: four boxes, the desktop instructions, and the address", () => {
    const h = render({ attach: false });
    needCode(h, "code_entry");

    expect(h.text(".fd-access__title")).toBe("Enter your code");
    expect(h.all(".fd-code__box")).toHaveLength(4);
    expect(h.text(".fd-access__body")).toContain("press Ctrl-g and run Web Interface");
    /** 2b's footer strip. The address is the one string here a user can check,
     * so it is printed from `location.host` and never invented. */
    expect(h.text(".fd-access__host")).toBe("no session · 192.168.2.14:7420");
  });

  it("code entry: digits fill the boxes and Enter submits a full code", () => {
    const h = render({ attach: false });
    needCode(h, "code_entry");
    for (const digit of ["8", "4", "1", "2"]) {
      h.key(digit);
    }
    expect(h.all(".fd-code__box").map((b) => b.textContent?.trim())).toEqual([
      "8",
      "4",
      "1",
      "2",
    ]);
    h.key("Enter");
    /** The exchange itself is injected — no test opens a socket or a fetch. */
    expect(h.submitted).toEqual(["8412"]);
  });

  it("code entry: Enter does nothing until the code is complete", () => {
    const h = render({ attach: false });
    needCode(h, "code_entry");
    h.key("8");
    h.key("Enter");
    expect(h.submitted).toEqual([]);
  });

  it("code entry: the caret sits on the first empty box", () => {
    const h = render({ attach: false });
    needCode(h, "code_entry");
    h.key("8");
    const carets = h.all(".fd-code__box").map((b) => b.getAttribute("data-caret"));
    expect(carets).toEqual(["false", "true", "false", "false"]);
  });

  it("code entry: the keypad works without a keyboard", () => {
    /** 2a's QR points a phone here, and a phone has no keyboard to catch
     * `keydown` from until something is focused. */
    const h = render({ attach: false });
    needCode(h, "code_entry");
    const keys = h.all(".fd-code__key");
    expect(keys).toHaveLength(10);
    keys[7]?.click();
    expect(h.state().access?.code).toBe("7");
  });

  it("rejected: its own screen, the failed digits, and the two recovery steps", () => {
    const h = render({ attach: false });
    needCode(h, "code_entry");
    for (const digit of ["8", "4", "1", "9"]) {
      h.key(digit);
    }
    h.app.store.dispatch({
      type: "access/refused",
      screen: "rejected",
      attemptsRemaining: 3,
      lockoutSeconds: null,
    });

    expect(h.text(".fd-access__title")).toBe("That code did not work");
    expect(h.q(".fd-access").getAttribute("data-tone")).toBe("alert");
    expect(h.text(".fd-access__refused")).toContain("8419");
    expect(h.text(".fd-access__body")).toContain("120 seconds and only work once");
    const steps = h.all(".fd-access__steps li").map((li) => li.textContent);
    expect(steps).toEqual([
      "go to the machine running FlightDeck",
      "Ctrl-g → Web Interface → Space for a fresh code",
    ]);
    /** 2b's exact footer sentence, with the host's number in it. */
    expect(h.text(".fd-access__attempts")).toBe(
      "3 attempts left before this address is rate-limited for 60s",
    );
  });

  it("rate-limited: amber, no button, and the host's countdown", () => {
    const h = render({ attach: false });
    needCode(h, "rate_limited", { attemptsRemaining: 0, lockoutSeconds: 60 });

    expect(h.text(".fd-access__title")).toBe("Too many attempts");
    /** Amber, by the same logic 2b applies to `revoked`: a limiter doing its
     * job is not a failure. */
    expect(h.q(".fd-access").getAttribute("data-tone")).toBe("stale");
    expect(h.text(".fd-access__body")).toContain("Try again in 60s");
    /** No primary button: the host would refuse it, and offering an action we
     * know cannot work is exactly the claim Q7 forbids. */
    expect(h.maybe(".fd-access__primary")).toBeNull();
    /** And no code boxes, because typing into them would be pointless. */
    expect(h.all(".fd-code__box")).toHaveLength(0);
    expect(h.text(".fd-access__attempts")).toContain("rate-limited for another 60s");
  });

  it("revoked: amber, from the desktop, and the photograph underneath", () => {
    const h = render();
    h.app.store.dispatch({ type: "access/revoked", revokedAgo: "12s ago" });

    expect(h.text(".fd-access__title")).toBe("Access revoked");
    expect(h.text(".fd-access__eyebrow")).toBe("from the desktop");
    expect(h.q(".fd-access").getAttribute("data-tone")).toBe("stale");
    expect(h.text(".fd-access__body")).toContain("withdrew this browser's access 12s ago");
    expect(h.text(".fd-access__detail")).toContain("The agents kept running");
    /** 2b draws the panel over live output, and says so in words. */
    expect(h.q(".fd-pane").getAttribute("data-tone")).toBe("stale");
    /** Both offers: a new code, or stay and read the photograph. */
    expect(h.text(".fd-access__primary")).toContain("Enter a new code");
    expect(h.text(".fd-access__secondary")).toContain("Stay here");
  });

  it("revoked: Esc stays here without claiming to be authorised", () => {
    const h = render();
    h.app.store.dispatch({ type: "access/revoked", revokedAgo: "12s ago" });
    h.key("Escape");
    expect(h.q(".fd-access").hidden).toBe(true);
    /** Nothing about the credential changed, so the strip must still say so. */
    expect(h.state().connection).toBe("revoked");
    expect(h.text(".fd-conn")).toContain("not allowed");
  });

  it("the access layer hides the two strips that have nothing honest to say", () => {
    const h = render({ attach: false });
    needCode(h, "code_entry");
    /** `app.el` *is* the frame, so this reads the attribute off the root. */
    expect(h.app.el.getAttribute("data-access")).toBe("true");
    /** With no session, the git bar and the status bar would be describing
     * nothing; 2b replaces both with its own footer strip. */
    expect(h.maybe(".fd-access__foot")).not.toBeNull();
  });

  it("swallows stray keys rather than letting them reach the terminal", () => {
    const h = render();
    needCode(h, "code_entry");
    const before = h.state().mode;
    h.key("q");
    expect(h.state().pendingInput).toEqual([]);
    /** Nothing reached the terminal, and nothing changed the mode underneath
     * the overlay either. */
    expect(h.state().mode).toBe(before);
  });

  it("leaves browser shortcuts alone", () => {
    /** Swallowing `Ctrl-a`/`Cmd-r` would cost the user select-all and reload,
     * two things a browser is entitled to keep. */
    const h = render({ attach: false });
    needCode(h, "code_entry");
    h.key("1", { ctrlKey: true });
    expect(h.state().access?.code).toBe("");
  });
});

/* ══ 2c — every connection state on the real bar ═════════════════════════ */

describe("2c — the connection strip", () => {
  const controlLosing: readonly ConnectionStatus[] = [
    "connecting",
    "reconnecting",
    "catching_up",
    "disconnected",
    "revoked",
  ];

  it("drains the mode chip on every state that costs the user control", () => {
    for (const status of controlLosing) {
      const h = render();
      h.app.store.dispatch({ type: "connection/changed", status });
      expect(h.text(".fd-mode")).toBe("MODE: —");
      expect(h.q(".fd-mode").getAttribute("data-tone")).toBe("drained");
      document.body.replaceChildren();
    }
  });

  it("replaces rather than drains the chip when the host has exited (Q5)", () => {
    const h = render();
    h.app.store.dispatch({
      type: "connection/shutdown",
      shutdown: {
        reason: "host_quit",
        selfInitiated: false,
        detail: "",
        atLabel: "16:42",
      },
    });
    /** 2c: `FLIGHTDECK STOPPED`. "No mode" understates a host that is gone —
     * and either way, no state that costs control ever names a mode. */
    expect(h.text(".fd-mode")).toBe("FLIGHTDECK STOPPED");
    expect(h.text(".fd-mode")).not.toContain("MODE: TERMINAL");
  });

  it("drains the chip for a read-only seat, and says why", () => {
    const h = render();
    h.app.store.dispatch({
      type: "seats/changed",
      seats: h.state().seats,
      seat: "observing",
    });
    expect(h.text(".fd-mode")).toBe("MODE: —");
    expect(h.text(".fd-statusbar")).toContain("read-only");
  });

  it("keeps the mode chip on a version mismatch — nothing was lost but the version", () => {
    const h = render();
    h.app.store.dispatch({
      type: "version/mismatch",
      mismatch: { tabVersion: "v1.16.0", hostVersion: "v1.17.0" },
    });
    expect(h.text(".fd-mode")).toBe("MODE: TERMINAL");
    expect(h.text(".fd-statusbar")).toContain("the host updated under you");
    expect(h.text(".fd-stripaction")).toContain("Reload for v1.17.0");
    expect(h.q(".fd-statusbar").getAttribute("data-frame")).toBe("info");
  });

  it("reload: Enter is scoped to the chip, not global (ll5.10, §6.5 R9)", () => {
    const h = render();
    h.app.store.dispatch({
      type: "version/mismatch",
      mismatch: { tabVersion: "v1.16.0", hostVersion: "v1.17.0" },
    });
    /** The mode is still `terminal` (2c keeps the chip intact), and an
     * `Enter` typed there — or anywhere else that is not the chip itself —
     * must stay a newline/focus keystroke, never a reload. `h.key` dispatches
     * at the frame, which is exactly what a keystroke *not* aimed at the chip
     * looks like: it reaches every global branch and none of them is this
     * one. */
    expect(h.state().mode).toBe("terminal");
    h.key("Enter");
    expect(h.actions).toEqual([]);

    /** Pressed at the chip itself — the shape a real focused `Enter` takes —
     * it does fire, and the chip says in its title that this is the only way
     * it works. */
    const button = h.q(".fd-stripaction");
    expect(button.title).toBe("Enter reloads only while this button is focused");
    button.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(h.actions.map((a) => a.kind)).toEqual(["reload"]);
  });

  it("never moves the connection strip's position", () => {
    /**
     * 2c's first rule. The mechanism is structural: the flexible spacer is
     * always the element immediately before `.fd-conn`, in every state, so the
     * group is always pushed to the same edge. Asserting the mechanism catches
     * a regression that a pixel comparison would not explain.
     */
    for (const status of ALL_CONNECTION_STATUSES) {
      const h = render();
      h.app.store.dispatch({ type: "connection/changed", status });
      /** Scoped to the bar: `.fd-spacer` is a generic flexible gap used in the
       * project row and the feed header too. */
      const spacer = h.q(".fd-statusbar .fd-spacer");
      expect(spacer.nextElementSibling?.classList.contains("fd-conn")).toBe(true);
      document.body.replaceChildren();
    }
  });

  it("takes the state's frame colour, and only ever one at a time", () => {
    const expected: readonly (readonly [ConnectionStatus, string])[] = [
      ["connected", "neutral"],
      ["connecting", "neutral"],
      ["catching_up", "neutral"],
      ["reconnecting", "stale"],
      ["disconnected", "alert"],
      ["revoked", "stale"],
      ["stopped", "stopped"],
    ];
    for (const [status, frame] of expected) {
      const h = render();
      h.app.store.dispatch({ type: "connection/changed", status });
      expect(h.q(".fd-statusbar").getAttribute("data-frame")).toBe(frame);
      document.body.replaceChildren();
    }
  });

  it("offers `r Retry now` when disconnected, from the keyboard too", () => {
    const h = render();
    h.app.store.dispatch({ type: "connection/changed", status: "disconnected" });
    expect(h.text(".fd-stripaction")).toContain("Retry now");
    h.q(".fd-stripaction").click();
    h.key("r");
    /** Once from the click, once from the key. */
    expect(h.actions.map((a) => a.kind)).toEqual(["retry", "retry"]);
  });

  it("offers a code when revoked, from the keyboard too", () => {
    const h = render();
    h.app.store.dispatch({ type: "connection/changed", status: "revoked" });
    expect(h.text(".fd-stripaction")).toContain("Enter a code");
    h.key("Enter");
    expect(h.state().access?.screen).toBe("code_entry");
  });

  it("names the seats instead of counting them (2f)", () => {
    const h = render();
    expect(h.text(".fd-viewers")).toBe("desktop + this tab");
    expect(h.q(".fd-viewers").title).toContain("controls input");
  });
});

/* ══ 2d — live · asleep · stale · asleep-and-stale · catching up ══════════ */

describe("2d — the pane treatments", () => {
  it("keeps all five mutually distinguishable in the DOM", () => {
    const seen = new Map<string, string>();
    const cases: readonly (readonly [string, () => Harness])[] = [
      [
        "live",
        () => render(),
      ],
      [
        "asleep",
        () => {
          const h = render();
          h.app.store.dispatch({ type: "mode/set", mode: "app" });
          return h;
        },
      ],
      [
        "stale",
        () => {
          const h = render();
          h.app.store.dispatch({ type: "connection/changed", status: "reconnecting" });
          return h;
        },
      ],
      [
        "asleep_stale",
        () => {
          const h = render();
          h.app.store.dispatch({ type: "mode/set", mode: "app" });
          h.app.store.dispatch({ type: "connection/changed", status: "reconnecting" });
          return h;
        },
      ],
      [
        "catching_up",
        () => {
          const h = render();
          h.app.store.dispatch({ type: "connection/changed", status: "catching_up" });
          return h;
        },
      ],
    ];
    for (const [name, build] of cases) {
      const h = build();
      const pane = h.q(".fd-pane");
      const tone = pane.getAttribute("data-tone") ?? "";
      const caret = pane.getAttribute("data-caret") ?? "";
      const scanlines = String(!h.q(".fd-pane__scanlines").hidden);
      const signature = `${tone}|${caret}|${scanlines}`;
      /** Every one of the five has to be a different signature — that is what
       * "three visually distinct states" means once it is testable. */
      expect(seen.has(signature)).toBe(false);
      seen.set(signature, name);
      document.body.replaceChildren();
    }
    expect(seen.size).toBe(5);
  });

  it("stale removes the caret and shows the frozen clock", () => {
    const h = render();
    h.app.store.dispatch({
      type: "staleness/set",
      staleness: { frozenAt: "16:41:08", ago: "34s" },
    });
    h.app.store.dispatch({ type: "connection/changed", status: "reconnecting" });

    const pane = h.q(".fd-pane");
    expect(pane.getAttribute("data-tone")).toBe("stale");
    /** A blinking caret is the strongest "I am listening" signal a terminal
     * has, so on a photograph it goes away entirely. */
    expect(pane.getAttribute("data-caret")).toBe("off");
    expect(h.q(".fd-pane__scanlines").hidden).toBe(false);
    expect(h.text(".fd-pane__clock")).toBe("16:41:08");
    expect(h.text(".fd-pane__banner")).toContain("frozen 34s ago");
    expect(h.text(".fd-pane__banner")).toContain("this is a photograph");
  });

  it("asleep keeps the caret and the picture's truth", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    const pane = h.q(".fd-pane");
    expect(pane.getAttribute("data-tone")).toBe("asleep");
    expect(pane.getAttribute("data-caret")).toBe("on");
    expect(h.q(".fd-pane__scanlines").hidden).toBe(true);
    /** Asleep means the keys went elsewhere — and says where. */
    expect(h.text(".fd-pane__foot")).toContain("keystrokes go to FlightDeck");
  });

  it("asleep-and-stale keeps the scanlines and changes the words", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    h.app.store.dispatch({
      type: "staleness/set",
      staleness: { frozenAt: "16:41:08", ago: "34s" },
    });
    h.app.store.dispatch({ type: "connection/changed", status: "reconnecting" });

    expect(h.q(".fd-pane").getAttribute("data-tone")).toBe("asleep_stale");
    /** The scanlines are what survive both, which is what makes this a third
     * state rather than an ambiguous blend of two. */
    expect(h.q(".fd-pane__scanlines").hidden).toBe(false);
    expect(h.text(".fd-pane__banner")).toContain(
      "FlightDeck has focus, so keys would go to the sidebar anyway",
    );
  });

  it("shows how many keystrokes are being held (§5.1)", () => {
    const h = render();
    h.app.store.dispatch({ type: "connection/changed", status: "reconnecting" });
    h.app.store.dispatch({ type: "input/queue", data: "a" });
    h.app.store.dispatch({ type: "input/queue", data: "b" });
    /** A promise with a number on it is one a user can check. */
    expect(h.text(".fd-pane__held")).toBe("2 keystrokes held");
    expect(h.text(".fd-statusbar")).toContain("keystrokes are being held");
  });

  it("catching up shows the replay bar and Q3's cursor", () => {
    const h = render();
    h.app.store.dispatch({
      type: "replay/set",
      replay: {
        bytesDone: 25_000,
        bytesTotal: 41_984,
        fromByte: 1_204_992,
        truncated: false,
      },
    });
    h.app.store.dispatch({ type: "connection/changed", status: "catching_up" });

    const bar = h.q(".fd-replay");
    expect(bar.getAttribute("value")).toBe("25000");
    expect(bar.getAttribute("max")).toBe("41984");
    expect(h.text(".fd-pane__banner")).toContain("replaying 17 KB…");
    expect(h.text(".fd-pane__banner")).toContain("from byte 1 204 992");
    /** Colour is back, so the picture is trustworthy: no scanlines. */
    expect(h.q(".fd-pane__scanlines").hidden).toBe(true);
  });

  it("says so when the replay is not continuous (Q3)", () => {
    const h = render();
    h.app.store.dispatch({
      type: "replay/set",
      replay: { bytesDone: 0, bytesTotal: 4096, fromByte: 0, truncated: true },
    });
    h.app.store.dispatch({ type: "connection/changed", status: "catching_up" });
    expect(h.text(".fd-pane__banner")).toContain("not a continuous replay");
  });
});

/* ══ 2f — the takeover trio ══════════════════════════════════════════════ */

describe("2f — takeover", () => {
  const incumbent = {
    address: "192.168.2.20",
    browser: "Safari · iOS 18",
    connected: "14 minutes, active 20s ago",
  };

  function arriving(h: Harness): void {
    h.app.store.dispatch({ type: "takeover/held", incumbent });
  }

  function evicted(h: Harness): void {
    h.app.store.dispatch({
      type: "takeover/evicted",
      byAddress: "192.168.2.11",
      lastInputAgo: "3s",
    });
  }

  it("arriving: names the incumbent and offers all three ways out", () => {
    const h = render();
    arriving(h);
    expect(h.text(".fd-takeover__title")).toBe("Someone else is driving");
    expect(h.text(".fd-takeover__facts")).toContain("192.168.2.20");
    expect(h.text(".fd-takeover__facts")).toContain("Safari · iOS 18");
    expect(h.text(".fd-takeover__facts")).toContain("14 minutes, active 20s ago");
    /** D14: courtesy, not a permission check. */
    expect(h.text(".fd-takeover__detail")).toContain("it is not a lock");
    expect(h.all(".fd-takeover__action").map((b) => b.getAttribute("data-key"))).toEqual([
      "Enter",
      "w",
      "Esc",
    ]);
  });

  it("arriving: taking over claims the seat", () => {
    const h = render();
    arriving(h);
    h.key("Enter");
    expect(h.state().seat).toBe("controlling");
    expect(h.q(".fd-takeover").hidden).toBe(true);
  });

  it("arriving: cancelling leaves a live read-only view", () => {
    const h = render();
    arriving(h);
    h.key("Escape");
    /** 2f: "cancelling still leaves a live read-only view" — observation costs
     * the host nothing and answers "is it done yet?" without evicting anyone. */
    expect(h.state().seat).toBe("observing");
    expect(h.state().connection).toBe("connected");
    expect(h.q(".fd-pane").getAttribute("data-tone")).toBe("live");
    expect(h.text(".fd-mode")).toBe("MODE: —");
  });

  it("evicted: a prompt over a live connection, never a shutdown", () => {
    const h = render();
    evicted(h);
    expect(h.text(".fd-takeover__title")).toBe("Another browser took over");
    expect(h.text(".fd-takeover__body")).toContain("192.168.2.11");
    expect(h.text(".fd-takeover__body")).toContain("3s");
    /** Eviction is a `Delta::Seats`; the socket stays open. */
    expect(h.state().connection).toBe("connected");
    expect(h.state().shutdown).toBeNull();
    /** And what is behind the dialog is stale from the moment control was lost. */
    expect(h.q(".fd-pane").getAttribute("data-tone")).toBe("stale");
    expect(h.text(".fd-takeover__stale")).toContain("stale from the moment you lost control");
  });

  it("evicted: can watch instead of fighting, and the view goes live again", () => {
    const h = render();
    evicted(h);
    h.key("w");
    expect(h.state().seat).toBe("observing");
    /** Colour means live (2d), and an observer really is getting bytes. */
    expect(h.q(".fd-pane").getAttribute("data-tone")).toBe("live");
    expect(h.q(".fd-takeover").hidden).toBe(true);
  });

  it("evicted: can take it back without a trip to the desktop", () => {
    const h = render();
    evicted(h);
    expect(h.text(".fd-takeover__action--primary")).toContain("Take it back");
    h.q(".fd-takeover__action--primary").click();
    expect(h.state().seat).toBe("controlling");
  });

  it("evicted: offers no Cancel, because there is nothing to cancel", () => {
    const h = render();
    evicted(h);
    expect(h.all(".fd-takeover__action").map((b) => b.getAttribute("data-key"))).toEqual([
      "Enter",
      "w",
    ]);
  });

  it("reports the takeover intent to the wire seam", () => {
    const h = render();
    arriving(h);
    h.key("Enter");
    /** There is no takeover frame in v1: the caller re-sends `Attach { seat }`,
     * which is why this is reported rather than sent from the component. */
    expect(h.dispatched.map((a) => a.type)).toContain("takeover/claim");
  });
});

/* ══ 2e — the activity feed ══════════════════════════════════════════════ */

describe("2e — the activity feed", () => {
  it("opens with `a` in App mode and closes with it again", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    h.key("a");
    expect(h.state().feedOpen).toBe(true);
    expect(h.q(".fd-feed").hidden).toBe(false);
    h.key("a");
    expect(h.state().feedOpen).toBe(false);
  });

  it("does not steal `a` in Terminal mode, where it is a letter", () => {
    const h = render();
    h.key("a");
    expect(h.state().feedOpen).toBe(false);
  });

  it("opens from the unread chip, for a device with no `a` key", () => {
    const h = render();
    h.q(".fd-unread").click();
    expect(h.state().feedOpen).toBe(true);
  });

  it("is a slide-over, not a modal", () => {
    const h = render();
    h.app.store.dispatch({ type: "feed/set", open: true });
    const feed = h.q(".fd-feed");
    /**
     * D11 makes this the only notification channel there is, so it must never
     * block the screen: a complementary landmark with no `aria-modal`, and the
     * terminal behind it still present and untouched.
     */
    expect(feed.getAttribute("role")).toBe("complementary");
    expect(feed.getAttribute("aria-modal")).toBeNull();
    expect(feed.getAttribute("aria-hidden")).toBeNull();
    const pane = h.q(".fd-pane");
    expect(pane.hidden).toBe(false);
    expect(pane.getAttribute("aria-hidden")).toBeNull();
    expect(pane.hasAttribute("inert")).toBe(false);
  });

  it("backfills from the host's store, so a fresh tab opens on history", () => {
    const h = render();
    h.app.store.dispatch({ type: "feed/set", open: true });
    const rows = h.all(".fd-feed__row");
    /** The tab was not watching when any of these happened. */
    expect(rows).toHaveLength(fixtureActivity().length);
    expect(h.text(".fd-feed__foot")).toContain("200 events / 24h");
    /** Newest first, while the host backfills oldest first. */
    expect(rows[0]?.textContent).toContain("rotate-jwt-secret");
  });

  it("renders the reason the host sent, including `unknown → unknown`", () => {
    const h = render();
    h.app.store.dispatch({ type: "feed/set", open: true });
    const text = h.text(".fd-feed__list");
    expect(text).toContain("unknown → unknown · Codex CLI reports no lifecycle");
    expect(text).toContain("in progress → waiting · asked a question");
    expect(text).toContain("in progress → idle · finished, 18 files touched");
  });

  it("says on every row that a jump also moves the desktop (D3)", () => {
    const h = render();
    h.app.store.dispatch({ type: "feed/set", open: true });
    for (const row of h.all(".fd-feed__row")) {
      expect(row.title).toBe("jump · also moves the desktop");
      /** Also as text, because a `title` is invisible on a phone — which is
       * the device this slide-over shape exists for. */
      expect(row.textContent).toContain("jump · also moves the desktop");
    }
  });

  it("a row selects its session, across projects, and closes the feed", () => {
    const h = render();
    h.app.store.dispatch({ type: "feed/set", open: true });
    /** Newest first, and the newest fixture row lives in another project. */
    h.all(".fd-feed__row")[0]?.click();
    expect(h.state().selection).toMatchObject({
      projectId: "p-api-gateway",
      sessionId: "s-rotate-jwt-secret",
    });
    expect(h.state().feedOpen).toBe(false);
    /** D3's cost, reported to the wire seam so the desktop follows. */
    expect(h.dispatched.map((a) => a.type)).toContain("selection/jump");
  });

  it("clicking a row does not also release the keys", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "terminal" });
    h.app.store.dispatch({ type: "feed/set", open: true });
    h.all(".fd-feed__row")[0]?.click();
    /** An overlay is over the terminal, not outside it: every choice made in
     * one would otherwise quietly change the mode underneath. */
    expect(h.state().mode).toBe("terminal");
  });

  it("shows the three unread tiers in precedence order, and the all-read state", () => {
    const h = render();
    /** The fixture has two attention rows and one finished row unread. */
    expect(h.q(".fd-unread").getAttribute("data-tier")).toBe("attention");
    expect(h.text(".fd-unread")).toBe("▲ 2 need you");

    h.app.store.dispatch({
      type: "activity/read",
      ids: h.state().activity.filter((e) => e.tier === "attention").map((e) => e.id),
    });
    expect(h.q(".fd-unread").getAttribute("data-tier")).toBe("finished");
    expect(h.text(".fd-unread")).toBe("▲ 1 finished");

    h.app.store.dispatch({
      type: "activity/received",
      events: [
        {
          id: "e-quiet",
          atLabel: "now",
          projectId: "p-web",
          projectName: "web",
          sessionId: "s-bump-deps",
          sessionName: "bump-deps",
          from: "idle",
          to: "reviewing",
          reason: "set by hand on the desktop",
          tier: "quiet",
          read: false,
        },
      ],
    });
    /** Finished still beats quiet. */
    expect(h.q(".fd-unread").getAttribute("data-tier")).toBe("finished");

    h.app.store.dispatch({
      type: "activity/read",
      ids: h.state().activity.map((e) => e.id),
    });
    /** All read: the affordance stays, the claim goes. */
    const chip = h.q(".fd-unread");
    expect(chip.getAttribute("data-tier")).toBe("read");
    expect(chip.textContent).toContain("▵ activity");
  });

  it("opening the feed marks what it shows as read", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    h.key("a");
    expect(h.q(".fd-unread").getAttribute("data-tier")).toBe("read");
    /** The host owns the authoritative record; this is the local half. */
    expect(h.dispatched.map((a) => a.type)).toContain("activity/read");
  });

  it("has an empty state that says what would land there", () => {
    const h = render({ attach: false });
    h.app.store.dispatch({ type: "feed/set", open: true });
    expect(h.text(".fd-feed__empty-title")).toBe("Nothing has changed in 24 hours.");
    expect(h.text(".fd-feed__empty-body")).toContain("stalling on a question");
  });

  it("advertises `a activity` in App mode's hint row", () => {
    const h = render();
    h.app.store.dispatch({ type: "mode/set", mode: "app" });
    expect(h.text(".fd-statusbar")).toContain("a activity");
  });

  it("closes on Escape without dropping terminal focus", () => {
    const h = render();
    h.app.store.dispatch({ type: "feed/set", open: true });
    h.key("Escape");
    expect(h.state().feedOpen).toBe(false);
    expect(h.state().mode).toBe("terminal");
  });
});

describe("the overlays stay keyboard-operable", () => {
  it("never swallows Tab", () => {
    /**
     * Both overlays are operable by pointer *and* by keyboard — every button in
     * them is a real button — so eating `Tab` would leave a keyboard-only user
     * with a panel they can see and cannot reach.
     */
    for (const open of [
      (h: Harness) => {
        h.app.store.dispatch({
          type: "access/required",
          screen: "code_entry",
          attemptsRemaining: null,
          lockoutSeconds: null,
        });
      },
      (h: Harness) => {
        h.app.store.dispatch({
          type: "takeover/held",
          incumbent: { address: "a", browser: "b", connected: "c" },
        });
      },
    ]) {
      const h = render();
      open(h);
      const event = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
      h.app.el.dispatchEvent(event);
      expect(event.defaultPrevented).toBe(false);
      document.body.replaceChildren();
    }
  });
});
