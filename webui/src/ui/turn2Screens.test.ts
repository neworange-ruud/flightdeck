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
import { incumbentFromSeats } from "./takeover";

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
  /**
   * `extra` defaults `lockoutLengthSeconds`/`codeTtlSeconds` to the numbers the
   * shipped host sends, because they are *host-sent* now rather than mirrored
   * here — the tests that matter are the two that pass `null` explicitly and
   * assert the sentence loses its clause instead of gaining an invention.
   */
  function needCode(
    h: Harness,
    screen: "code_entry" | "rejected" | "rate_limited",
    extra: {
      attemptsRemaining?: number;
      lockoutSeconds?: number;
      lockoutLengthSeconds?: number | null;
      codeTtlSeconds?: number | null;
    } = {},
  ): void {
    h.app.store.dispatch({
      type: "access/required",
      screen,
      attemptsRemaining: extra.attemptsRemaining ?? null,
      lockoutSeconds: extra.lockoutSeconds ?? null,
      lockoutLengthSeconds:
        extra.lockoutLengthSeconds === undefined ? 60 : extra.lockoutLengthSeconds,
      codeTtlSeconds: extra.codeTtlSeconds === undefined ? 120 : extra.codeTtlSeconds,
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
      /** The host's `RATE_LIMIT_LOCKOUT_MS` and `BOOTSTRAP_CODE_TTL_MS`, sent
       * on every refusal. The browser no longer keeps copies of them. */
      lockoutLengthSeconds: 60,
      codeTtlSeconds: 120,
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

  it("prints the two policy numbers the host sent, not numbers of its own", () => {
    const h = render({ attach: false });
    /** A host tuned away from the shipped 60/120. The screens follow it,
     * because they no longer hold copies of those constants. */
    needCode(h, "rejected", {
      attemptsRemaining: 2,
      lockoutLengthSeconds: 90,
      codeTtlSeconds: 45,
    });
    expect(h.text(".fd-access__body")).toContain("Codes last 45 seconds");
    expect(h.text(".fd-access__attempts")).toBe(
      "2 attempts left before this address is rate-limited for 90s",
    );
  });

  it("drops the clause rather than inventing a number the host did not send", () => {
    /**
     * Honest degradation, and the reason all four numbers are nullable: a host
     * that says nothing leaves each sentence one clause shorter and still true.
     * Filling the gap from a remembered constant would be the browser asserting
     * a policy it does not set.
     */
    const h = render({ attach: false });
    needCode(h, "rejected", {
      attemptsRemaining: 2,
      lockoutLengthSeconds: null,
      codeTtlSeconds: null,
    });
    expect(h.text(".fd-access__body")).toBe(
      "It expired, or it was mistyped. Codes only work once.",
    );
    expect(h.text(".fd-access__attempts")).toBe(
      "2 attempts left before this address is rate-limited",
    );

    needCode(h, "code_entry", { codeTtlSeconds: null });
    expect(h.text(".fd-access__body")).toContain("The overlay shows a four-digit code.");
    expect(h.text(".fd-access__body")).not.toContain("minutes");
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

  it("revoked: says only that it happened when the host did not say when", () => {
    /** 2b's sentence without its clause. A zero or a guessed duration on a
     * security screen would be the first lie the user is shown. */
    const h = render();
    h.app.store.dispatch({ type: "access/revoked", revokedAgo: null });
    const body = h.text(".fd-access__body");
    expect(body).toContain("withdrew this browser's access.");
    expect(body).toContain("Nothing is broken and nothing is lost");
    expect(body).not.toContain("ago");
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
    expect(h.text(".fd-viewers")).toBe("desktop + this tab ✎");
    /** The second fact D14's revision made necessary: both surfaces may type,
     * and the `✎` says which one is typing right now. */
    expect(h.q(".fd-viewers").title).toContain("desktop — can type");
    expect(h.q(".fd-viewers").title).toContain("this tab — typing now");
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

  it("arriving: with no seat list yet, names the incumbent from the refusal", () => {
    /**
     * `WireError::seat_held` arrives before any dated seat list on a fresh
     * socket, so the panel really can open knowing one writer and nothing else.
     * `attach: false` is that situation: no snapshot, so no rows.
     */
    const h = render({ attach: false });
    arriving(h);
    expect(h.text(".fd-takeover__title")).toBe("Someone else is typing");
    expect(h.text(".fd-takeover__facts")).toContain("192.168.2.20");
    expect(h.text(".fd-takeover__facts")).toContain("Safari · iOS 18");
    expect(h.text(".fd-takeover__facts")).toContain("14 minutes, active 20s ago");
    /** One writer described is not a roster, and must not be drawn as one: a
     * one-row list would claim the reader is alone with that writer. */
    expect(h.maybe(".fd-takeover__seats")).toBeNull();
    /** The panel says what actually happened: refused rather than mixed in, and
     * waiting is a real option because the lock frees itself. */
    expect(h.text(".fd-takeover__body")).toContain("one cursor");
    expect(h.text(".fd-takeover__body")).toContain("frees itself");
    /** D14: courtesy, not a permission check — and no surface has precedence. */
    expect(h.text(".fd-takeover__detail")).toContain("the desktop plays by the same rule");
    expect(h.all(".fd-takeover__action").map((b) => b.getAttribute("data-key"))).toEqual([
      "Enter",
      "w",
      "Esc",
    ]);
  });

  it("arriving: each of the three facts comes from its own field", () => {
    /**
     * The panel used to get the merged `SeatInfo::label` in the address slot and
     * nothing in the browser slot — deliberately, because splitting untrusted
     * display text on a separator is a parse the text itself can steer. The fix
     * was on the wire: the host now sends `address` and `user_agent_label`
     * apart, and the label stays for the compact chip.
     */
    const h = render();
    h.app.store.dispatch({
      type: "seats/changed",
      seat: "writing",
      seats: [
        {
          label: "desktop",
          address: null,
          browser: null,
          seat: "writing",
          holdsInput: false,
          isDesktop: true,
          isYou: false,
          sinceLabel: "since launch",
        },
        {
          label: "192.168.2.20 · Safari · iOS 18",
          address: "192.168.2.20",
          /** A separator inside the browser's own claim: exactly the payload a
           * browser-side split of the label would get wrong. */
          browser: "Safari · iOS 18",
          seat: "writing",
          holdsInput: true,
          isDesktop: false,
          isYou: false,
          sinceLabel: "14 minutes",
        },
      ],
    });

    expect(incumbentFromSeats(h.state().seats)).toEqual({
      address: "192.168.2.20",
      browser: "Safari · iOS 18",
      connected: "14 minutes",
    });
  });

  it("arriving: an older host's merged label still names an address, and no browser", () => {
    /** Honest degradation. With no `address`/`user_agent_label` on the wire the
     * merged label is all we have — it is a true answer for the address slot,
     * which it starts with — and the browser row is dropped rather than filled
     * from a split we refuse to perform. */
    const h = render();
    h.app.store.dispatch({
      type: "seats/changed",
      seat: "writing",
      seats: [
        {
          label: "192.168.2.20 · Safari on iOS",
          address: null,
          browser: null,
          seat: "writing",
          holdsInput: true,
          isDesktop: false,
          isYou: false,
          sinceLabel: "14 minutes",
        },
      ],
    });

    expect(incumbentFromSeats(h.state().seats)).toEqual({
      address: "192.168.2.20 · Safari on iOS",
      browser: "",
      connected: "14 minutes",
    });
  });

  it("arriving: a fact the host did not send is a row that is not drawn", () => {
    const h = render({ attach: false });
    h.app.store.dispatch({
      type: "takeover/held",
      incumbent: { address: "192.168.2.20", browser: "", connected: "" },
    });
    const rows = h.all(".fd-takeover__facts dt").map((dt) => dt.textContent);
    expect(rows).toEqual(["address"]);
    expect(h.text(".fd-takeover__facts")).toContain("192.168.2.20");
  });

  it("arriving: taking over asks for the writer's seat and the turn", () => {
    const h = render();
    arriving(h);
    h.key("Enter");
    expect(h.state().seat).toBe("writing");
    expect(h.q(".fd-takeover").hidden).toBe(true);
  });

  it("arriving: being refused costs the turn, never the seat", () => {
    /**
     * D14 as revised. v1 answered `seat_held` by dropping to read-only, because
     * the seat really had been taken. Now the seat is untouched — draining the
     * mode chip here would tell a tab that is still a writer, and will be
     * typing again in 400ms, that it has lost control.
     */
    const h = render();
    arriving(h);
    expect(h.state().seat).toBe("writing");
    expect(h.text(".fd-mode")).toBe("MODE: TERMINAL");
  });

  it("arriving: cancelling leaves a live view, and the seat we still have", () => {
    const h = render();
    arriving(h);
    h.key("Escape");
    /**
     * 2f: "cancelling still leaves a live view" — and under D14 as revised a
     * live *writing* one. v1 folded `Esc` into `w` correctly, because a refusal
     * then meant the seat was gone and read-only was all that was left. A
     * refusal now costs the turn only, so cancelling means "I will wait": the
     * lock comes back on its own once the other writer goes quiet, and taking
     * the seat away here would remove something the host never took.
     */
    expect(h.state().seat).toBe("writing");
    expect(h.state().connection).toBe("connected");
    expect(h.q(".fd-pane").getAttribute("data-tone")).toBe("live");
    expect(h.text(".fd-mode")).toBe("MODE: TERMINAL");
  });

  it("arriving: `w` is the way to stop competing, and it is a different act", () => {
    const h = render();
    arriving(h);
    h.key("w");
    expect(h.state().seat).toBe("observing");
    expect(h.text(".fd-mode")).toBe("MODE: —");
  });

  it("evicted: a prompt over a live connection, never a shutdown", () => {
    const h = render();
    evicted(h);
    expect(h.text(".fd-takeover__title")).toBe("Someone took the input");
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
    expect(h.state().seat).toBe("writing");
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
    /** There is no takeover frame: the caller re-sends `Attach { seat }`,
     * which is why this is reported rather than sent from the component. */
    expect(h.dispatched.map((a) => a.type)).toContain("takeover/claim");
  });
});

/* ══ 2f, widened for M3 — the seat panel as rows ═════════════════════════ */

/**
 * `remote-control-eek.3`. D14's second revision ends "M3's multi-viewer list is
 * the same panel with rows", and these are the properties that make the rows
 * worth having rather than a longer way of saying what the chip already says.
 *
 * The panel is opened here with `takeover/held` because that is what puts it on
 * screen; what is being asserted is the roster, which comes from `state.seats`
 * and not from the takeover state — which is exactly why it is live.
 */
describe("2f/M3 — the seat panel, N rows", () => {
  function seat(
    over: Partial<AppState["seats"][number]> = {},
  ): AppState["seats"][number] {
    return {
      label: "192.168.2.20 · Chrome on macOS",
      address: "192.168.2.20",
      browser: "Chrome on macOS",
      seat: "writing",
      holdsInput: false,
      isDesktop: false,
      isYou: false,
      sinceLabel: "4m ago",
      ...over,
    };
  }

  const desktop = seat({
    label: "desktop",
    address: null,
    browser: null,
    isDesktop: true,
    sinceLabel: "2h ago",
  });

  function panelWith(seats: readonly AppState["seats"][number][]): Harness {
    const h = render();
    h.app.store.dispatch({ type: "seats/changed", seat: "writing", seats });
    h.app.store.dispatch({
      type: "takeover/held",
      incumbent: { address: "192.168.2.11", browser: "", connected: "" },
    });
    return h;
  }

  function rowText(h: Harness): readonly string[] {
    return h.all(".fd-takeover__seat-label").map((el) => el.textContent ?? "");
  }

  it("draws one row per seat, in the host's order, and totals nothing", () => {
    /** Four surfaces is not an exotic case — it is one desktop and three tabs.
     * 2f names seats rather than counting them, and that reasoning gets
     * stronger with more rows, not weaker. */
    const h = panelWith([
      desktop,
      seat({ label: "192.168.2.11", isYou: true }),
      seat({ label: "192.168.2.12" }),
      seat({ label: "192.168.2.13", seat: "observing" }),
    ]);
    expect(rowText(h)).toEqual([
      "desktop",
      "192.168.2.11",
      "192.168.2.12",
      "192.168.2.13",
    ]);
    /** The fact list is what the roster replaced; both at once would say who is
     * typing twice, from two sources that can disagree. */
    expect(h.maybe(".fd-takeover__facts")).toBeNull();
  });

  it("marks the reader's own row, and only from the host's word", () => {
    /** Two tabs on one machine are two rows with the same address and the same
     * browser. Matching on either would mark both. */
    const mine = seat({ label: "192.168.2.11 · Chrome on macOS", isYou: true });
    const theirs = seat({ label: "192.168.2.11 · Chrome on macOS" });
    const h = panelWith([desktop, mine, theirs]);
    const marked = h.all(".fd-takeover__seat[data-you='true']");
    expect(marked).toHaveLength(1);
    expect(marked[0]?.textContent).toContain("this tab");
    /** `this tab` is the chip's own word for it — the same seat, described the
     * same way, wherever the reader meets it. */
    expect(h.all(".fd-takeover__seat-you").map((el) => el.textContent)).toEqual([
      "this tab",
    ]);
  });

  it("tells a writer apart from an observer, and neither is the turn", () => {
    const h = panelWith([
      desktop,
      seat({ label: "writer" }),
      seat({ label: "watcher", seat: "observing" }),
    ]);
    const roles = h
      .all(".fd-takeover__seat-role")
      .map((el) => el.textContent ?? "");
    expect(roles).toEqual(["can type", "can type", "read-only"]);
    /** An observer never contends, so it can never be the one holding it. */
    expect(h.all(".fd-takeover__seat[data-holds-input='true']")).toHaveLength(0);
  });

  it("says who is typing now, beside the writers who merely may", () => {
    /**
     * The renderable case D14's revision exists for, and the one protocol v1's
     * merged `controlling` flag could not express: **three writers, one of them
     * mid-burst.** The role and the turn are two marks because they are two
     * facts.
     */
    const h = panelWith([
      desktop,
      seat({ label: "192.168.2.11", holdsInput: true }),
      seat({ label: "192.168.2.12" }),
    ]);
    expect(
      h
        .all(".fd-takeover__seat-role")
        .map((el) => el.textContent ?? ""),
    ).toEqual(["can type", "typing now", "can type"]);
    const marks = h
      .all(".fd-takeover__seat-mark")
      .map((el) => el.textContent ?? "");
    expect(marks).toEqual(["", "✎", ""]);
  });

  it("follows a live Delta::Seats, holder and roster alike", () => {
    /** The rows are read from `state.seats` on every render, so a seat delta
     * repaints them. A reader watching the lock move sees it move — which is
     * the whole answer to "why did my keys stop working". */
    const h = panelWith([
      desktop,
      seat({ label: "192.168.2.11", holdsInput: true, isYou: true }),
    ]);
    expect(rowText(h)).toEqual(["desktop", "192.168.2.11"]);

    h.app.store.dispatch({
      type: "seats/changed",
      seat: "writing",
      seats: [
        { ...desktop, holdsInput: true },
        seat({ label: "192.168.2.11", isYou: true }),
        seat({ label: "192.168.2.12", seat: "observing" }),
      ],
    });

    expect(rowText(h)).toEqual(["desktop", "192.168.2.11", "192.168.2.12"]);
    /** The turn moved to the desktop — which is a legal answer, because the
     * desktop is one of the writers and has no precedence over any of them. */
    expect(
      h
        .all(".fd-takeover__seat[data-holds-input='true'] .fd-takeover__seat-label")
        .map((el) => el.textContent),
    ).toEqual(["desktop"]);
  });

  it("draws the joined-at, and drops it rather than fabricating one", () => {
    const h = panelWith([
      desktop,
      seat({ label: "dated", sinceLabel: "40s ago" }),
      /** A host that sent no clock to date its rows against. Empty is "we
       * cannot say", never "just now". */
      seat({ label: "undated", sinceLabel: "" }),
    ]);
    expect(
      h.all(".fd-takeover__seat-since").map((el) => el.textContent),
    ).toEqual(["connected 2h ago", "connected 40s ago"]);
  });

  it("puts the same rows on the evicted panel, which is the same panel", () => {
    const h = render();
    h.app.store.dispatch({
      type: "seats/changed",
      seat: "writing",
      seats: [desktop, seat({ label: "192.168.2.11", holdsInput: true })],
    });
    h.app.store.dispatch({
      type: "takeover/evicted",
      byAddress: "192.168.2.11",
      lastInputAgo: "3s ago",
    });
    expect(rowText(h)).toEqual(["desktop", "192.168.2.11"]);
    /** Losing the turn is exactly when "who else is here" becomes worth
     * knowing, and 2f's caption makes it one panel, not two. */
    expect(h.text(".fd-takeover__body")).toContain("3s ago");
  });

  it("leaves the clause out when no keystroke of ours ever landed", () => {
    /** A tab preempted before it typed has no "last one that landed" to date
     * the sentence from, and `just now` would be a time invented for an event
     * that did not happen. */
    const h = render();
    h.app.store.dispatch({
      type: "takeover/evicted",
      byAddress: "192.168.2.11",
      lastInputAgo: "",
    });
    expect(h.text(".fd-takeover__body")).not.toContain("the last one that landed");
    expect(h.text(".fd-takeover__body")).toContain("192.168.2.11");
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
          lockoutLengthSeconds: null,
          codeTtlSeconds: null,
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
