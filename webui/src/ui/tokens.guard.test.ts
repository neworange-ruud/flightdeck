import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * The cheapest possible defence of two rules that are otherwise only enforced
 * by whoever happens to review the diff.
 *
 * **Rule 1 — no colour literal (2g).** `src/style/tokens.css` is the only file
 * allowed to contain a colour *value*. Everything else names a token. A hex
 * value in a component is invisible in review, survives palette changes, and
 * quietly breaks the contrast ratios 2g spent a turn establishing.
 *
 * **Rule 2 — four type sizes (2g).** 11 meta / 12.5 body / 14 title / 30
 * pairing code, as `--fd-t-*`. Turn 1's seven sizes were "three sizes
 * pretending to be seven"; a raw `px` font-size anywhere else is a fifth size.
 *
 * The exemption list is asserted, not just applied, so that adding a second
 * exempt file is a visible change to this test rather than a quiet one-liner.
 *
 * **Rule 3 — the state and view layers never read a clock**
 * (`remote-control-ll5.8`). Three module docs already state this — `reduce` is
 * pure ("no `Date.now()`/`Math.random()`"), `state/model.ts` says a component
 * reading the clock "would be the same impurity one layer down", and
 * `wire/adapt.ts` says a host instant dated against a local clock is "a
 * confident guess". It was three prose rules and no check, which is the same
 * shape rules 1 and 2 were in before this file existed. The transport owns the
 * clock and injects it (`SessionSocket`'s `options.now`), and the access client
 * reads the host's own `server_time_ms`; nothing under `state/` or `ui/` may
 * ask this machine what time it is, because every time those layers render is
 * the **host's**.
 *
 * **Rule 4 — the SPA's `PROTOCOL_VERSION` matches the host's**
 * (`remote-control-ll5.8`). The two constants are a hand-maintained mirror
 * across two languages, and the whole point of the number is to catch a tab
 * that no longer matches its host — so the one thing it cannot survive is the
 * *mirror itself* going stale, which makes every version check pass while the
 * wires differ. Nothing caught that before this: `webui/e2e/chain.spec.ts`
 * would, but it is the Playwright job, which R6 registered **non-blocking
 * until 2026-09-10**. This is a file read and a regex, and it fails in
 * `npm run test`.
 *
 * **Rule 5 — the width decision is one pure function, and nothing measures
 * itself** (`remote-control-eek.4`, §6.5 R17). 1h's 900px boundary reaches CSS
 * as `data-width` on `.fd-frame`, from `widthClass`, from a pixel width
 * `main.ts` measures. Two things would quietly undo that:
 *
 *   - **a width media query.** `vitest` runs in jsdom, which parses `@media`
 *     and never evaluates it, so a layout that moved into one would be checked
 *     by nothing in `npm run test` — the same hole rule 4 was written for, and
 *     `webui/e2e/narrow.spec.ts` is the same non-blocking Playwright job. Only
 *     `prefers-reduced-motion` and other non-dimensional queries are allowed.
 *   - **a second measurement.** D4's position is that the browser *never*
 *     negotiates or requests a size, it only receives one; a component that
 *     asked how wide it was would be one refactor away from asking the host to
 *     match. `main.ts` is the single exempt file and it is not under `state/`
 *     or `ui/`, so the rule needs no exemption list at all.
 *
 * **Rule 6 — `hidden` means hidden, and one rule says so** (`eek.4`, R17).
 * Every overlay in this app is closed by setting `.hidden = true` and every one
 * of them sets `display: flex`, which outranks the UA stylesheet's
 * `[hidden] { display: none }`. A "closed" overlay therefore stayed laid out,
 * painted and — being `position: absolute` over the terminal — **hit-testable**,
 * eating clicks aimed at what was behind it with nothing on screen to show for
 * it. That was defended nine times by hand and missed five times; the fifth
 * miss cost a tablet user the ability to click the terminal at all.
 *
 * `app.css` now carries one document-level `[hidden] { display: none
 * !important }`, and this rule keeps it load-bearing: the rule exists, nothing
 * re-enables display on a `[hidden]` selector, and `!important` appears
 * nowhere else in `src/style/` where it could outrank it. Playwright found the
 * live instance; this is what finds the next one in `npm run test`.
 *
 * It also guards D4's `FitAddon` invariant, for the same reason: it is a rule
 * that a single well-meaning import would silently undo.
 */

const SRC = new URL("..", import.meta.url).pathname;

/** The palette's definitions live in exactly one file. */
const COLOUR_VALUE_EXEMPT = ["style/tokens.css"];
/**
 * The two files allowed to read this machine's clock, and what each reads it
 * for. Both are transport: `socket.ts` injects it as `options.now` so tests can
 * drive it, and `client.ts` uses it only against the host's own
 * `server_time_ms`. Neither is a place a rendered duration is computed.
 */
const CLOCK_EXEMPT = ["wire/socket.ts", "access/client.ts"];
/** This file quotes the forbidden patterns in order to forbid them. */
const SELF = "ui/tokens.guard.test.ts";

function sourceFiles(): readonly string[] {
  const found: string[] = [];
  const walk = (dir: string): void => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const full = join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(full);
      } else if (/\.(ts|css)$/.test(entry.name)) {
        found.push(relative(SRC, full));
      }
    }
  };
  walk(SRC);
  return found.sort();
}

const files = sourceFiles();
const read = (file: string): string => readFileSync(join(SRC, file), "utf8");

/**
 * Comments are stripped before the D4 checks below, so that a file is free to
 * *document* the invariant ("never import FitAddon", "no transform: scale") in
 * the words a reader needs, while the guard still fails on a real one.
 */
const readCode = (file: string): string =>
  read(file)
    .replace(/\/\*[\s\S]*?\*\//g, " ")
    .replace(/^\s*\/\/.*$/gm, " ");

describe("palette and type-scale guard", () => {
  it("finds the sources it is supposed to be guarding", () => {
    /** A guard that silently scanned nothing would pass forever. */
    expect(files.length).toBeGreaterThan(15);
    expect(files).toContain("style/main.css");
    expect(files).toContain("ui/sidebar.ts");
  });

  it("exempts only tokens.css from holding colour values", () => {
    expect(COLOUR_VALUE_EXEMPT).toEqual(["style/tokens.css"]);
  });

  it("contains no hex colour literal outside tokens.css", () => {
    const hex = /#[0-9a-fA-F]{3}(?:[0-9a-fA-F]{3})?(?:[0-9a-fA-F]{2})?\b/g;
    const offenders: string[] = [];
    for (const file of files) {
      if (COLOUR_VALUE_EXEMPT.includes(file) || file === SELF) {
        continue;
      }
      for (const match of read(file).matchAll(hex)) {
        offenders.push(`${file}: ${match[0]}`);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("contains no rgb()/hsl() colour literal outside tokens.css", () => {
    const fn = /\b(?:rgba?|hsla?)\(/g;
    const offenders: string[] = [];
    for (const file of files) {
      if (COLOUR_VALUE_EXEMPT.includes(file) || file === SELF) {
        continue;
      }
      for (const match of read(file).matchAll(fn)) {
        offenders.push(`${file}: ${match[0]}`);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("sets every CSS font-size from a --fd-t-* token", () => {
    /** Four sizes exist; a `px` here would be a fifth. */
    const declaration = /font-size\s*:\s*([^;}]+)/g;
    const offenders: string[] = [];
    for (const file of files.filter((f) => f.endsWith(".css"))) {
      if (COLOUR_VALUE_EXEMPT.includes(file)) {
        continue;
      }
      for (const match of read(file).matchAll(declaration)) {
        const value = (match[1] ?? "").trim();
        if (!value.startsWith("var(--fd-t-")) {
          offenders.push(`${file}: font-size: ${value}`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });

  it("never sets a numeric font size from TypeScript", () => {
    /** Covers both `fontSize: 13` (xterm's option) and
     * `element.style.fontSize = "13px"` (an inline-style escape hatch). */
    const numeric = /fontSize\s*[:=]\s*["']?\d/g;
    const offenders: string[] = [];
    for (const file of files.filter((f) => f.endsWith(".ts"))) {
      if (file === SELF) {
        continue;
      }
      for (const match of read(file).matchAll(numeric)) {
        offenders.push(`${file}: ${match[0]}`);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("exempts only the transport from reading the clock", () => {
    expect(CLOCK_EXEMPT).toEqual(["wire/socket.ts", "access/client.ts"]);
  });

  /**
   * Every duration these layers render is a *host* instant — `since_ms`,
   * `at_ms`, a seat's `connected 14 minutes` — and the host sends its own
   * `server_time_ms` beside each of them precisely so the arithmetic never
   * needs this machine's clock. A `Date.now()` here would silently substitute
   * a clock with no relationship to the host's, and would be wrong by however
   * far the two have drifted rather than failing outright.
   */
  it("reads no clock from the state or view layers", () => {
    const clock = /\bDate\.now\s*\(|\bnew\s+Date\s*\(/g;
    const offenders: string[] = [];
    for (const file of files) {
      if (
        file === SELF ||
        file.endsWith(".test.ts") ||
        CLOCK_EXEMPT.includes(file) ||
        !(file.startsWith("state/") || file.startsWith("ui/"))
      ) {
        continue;
      }
      for (const match of readCode(file).matchAll(clock)) {
        offenders.push(`${file}: ${match[0]}`);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("sets no inline style from a component", () => {
    /** Inline styles are how the first hex literal always gets in. Layout and
     * colour belong in `main.css`; components set classes and data-attributes. */
    const inline = /\.style\.[A-Za-z]/g;
    const offenders: string[] = [];
    for (const file of files.filter((f) => f.startsWith("ui/"))) {
      if (file === SELF) {
        continue;
      }
      for (const match of read(file).matchAll(inline)) {
        offenders.push(`${file}: ${match[0]}`);
      }
    }
    expect(offenders).toEqual([]);
  });
});

describe("the wire version mirror", () => {
  /**
   * `webui/src/wire/frames.ts` restates `src/web/protocol.rs`'s
   * `PROTOCOL_VERSION` because the SPA cannot import Rust. A mismatch is
   * invisible in review and silently defeats the very check the constant
   * exists for, so it is read out of both files and compared.
   */
  it("frames.ts declares the same PROTOCOL_VERSION as protocol.rs", () => {
    const rust = readFileSync(
      new URL("../../../src/web/protocol.rs", import.meta.url),
      "utf8",
    );
    const host = /pub const PROTOCOL_VERSION: u16 = (\d+);/.exec(rust);
    expect(host, "protocol.rs must declare PROTOCOL_VERSION").not.toBeNull();

    const tab = /export const PROTOCOL_VERSION = (\d+);/.exec(
      read("wire/frames.ts"),
    );
    expect(tab, "frames.ts must declare PROTOCOL_VERSION").not.toBeNull();

    expect(tab?.[1]).toBe(host?.[1]);
  });

  /** The host serves exactly one version (D9: server and SPA ship together),
   * so the floor and the ceiling move with it. A range that drifted open would
   * let a stale tab attach and then fail on the first frame it cannot parse. */
  it("the host advertises exactly its own version, with no range", () => {
    const rust = readFileSync(
      new URL("../../../src/web/protocol.rs", import.meta.url),
      "utf8",
    );
    const of = (name: string): string | undefined =>
      new RegExp(`pub const ${name}: u16 = (\\d+);`).exec(rust)?.[1];
    expect(of("MIN_SUPPORTED_VERSION")).toBe(of("PROTOCOL_VERSION"));
    expect(of("MAX_SUPPORTED_VERSION")).toBe(of("PROTOCOL_VERSION"));
  });
});

describe("the narrow layout is decided once, in one pure place", () => {
  /**
   * Everything below 900px is selected by `[data-width="narrow"]`, which jsdom
   * cannot evaluate either — but the *attribute* is set by `ui/app.ts` from
   * `AppState.width`, so `narrowScreen.test.ts` drives the whole layout by
   * dispatching a number. A `@media (max-width: …)` would put the same rules
   * somewhere no unit test can reach.
   */
  it("uses no width media query anywhere in the stylesheets", () => {
    const dimensional = /@media[^{]*\b(?:min|max)-(?:width|inline-size)\b/g;
    const offenders: string[] = [];
    for (const file of files.filter((f) => f.endsWith(".css"))) {
      /** Comments stripped first, so a stylesheet is free to *say* why it
       * does not use one — the same courtesy the `FitAddon` check extends. */
      for (const match of readCode(file).matchAll(dimensional)) {
        offenders.push(`${file}: ${match[0].trim()}`);
      }
    }
    expect(offenders).toEqual([]);
  });

  /** The narrow stylesheet has to actually be doing the work, or the rule
   * above passes by being vacuous. */
  it("keeps the narrow layout in a stylesheet that keys off the attribute", () => {
    const narrow = read("style/narrow.css");
    expect(narrow).toContain('[data-width="narrow"]');
    expect(files).toContain("style/narrow.css");
  });

  /**
   * D4 in the other direction: the browser receives a size, it never takes
   * one. `main.ts` reads `window.innerWidth` once and is neither `state/` nor
   * `ui/`, so this needs no exemption — which is the point of putting it
   * there.
   */
  it("measures nothing from the state or view layers", () => {
    const measure =
      /\b(?:innerWidth|innerHeight|outerWidth|matchMedia|getBoundingClientRect|ResizeObserver|offsetWidth|clientWidth)\b/g;
    const offenders: string[] = [];
    for (const file of files) {
      if (
        file === SELF ||
        file.endsWith(".test.ts") ||
        !(file.startsWith("state/") || file.startsWith("ui/"))
      ) {
        continue;
      }
      for (const match of readCode(file).matchAll(measure)) {
        offenders.push(`${file}: ${match[0]}`);
      }
    }
    expect(offenders).toEqual([]);
  });
});

describe("a closed overlay is closed", () => {
  /**
   * jsdom does no hit-testing, so `narrowScreen.test.ts` cannot see this class
   * of bug at all — a `hidden` overlay it queries is `hidden` as far as it is
   * concerned, while in a browser it is still swallowing every click behind
   * it. These three assertions are the unit-testable shadow of that.
   */
  it("declares the one document-level rule that makes it true", () => {
    /** Comments stripped, so the block comment explaining the rule does not
     * sit between it and the `}` this anchors on. */
    const base = readCode("style/app.css");
    /** A bare attribute selector: it has to apply to every element that is
     * ever given the attribute, not to a list somebody maintains. */
    const rule = /(^|\})\s*\[hidden\]\s*\{([^}]*)\}/.exec(base);
    expect(rule, "app.css must declare a bare [hidden] rule").not.toBeNull();
    expect(rule?.[2]).toMatch(/display\s*:\s*none\s*!important/);
  });

  it("never re-enables display on something that is hidden", () => {
    /** `.fd-feed[hidden] { display: flex }` would be the exact regression, and
     * it would look perfectly reasonable in a diff. */
    const offenders: string[] = [];
    for (const file of files.filter((f) => f.endsWith(".css"))) {
      for (const match of readCode(file).matchAll(
        /([^{}]*\[hidden\][^{}]*)\{([^{}]*)\}/g,
      )) {
        const display = /(?:^|;)\s*display\s*:\s*([^;!]+)/.exec(match[2] ?? "");
        if (display !== undefined && display !== null) {
          const value = display[1]?.trim();
          if (value !== "none") {
            offenders.push(`${file}: ${match[1]?.trim()} { display: ${value} }`);
          }
        }
      }
    }
    expect(offenders).toEqual([]);
  });

  it("keeps `!important` for that rule and nothing else", () => {
    /**
     * The rule only works because nothing can outrank it, and the only thing
     * that can is another `!important`. There is exactly one in the whole of
     * `src/style/`, and this is the assertion that keeps it that way — which
     * is also why `!important` is not a smell here: it is the mechanism.
     */
    const found: string[] = [];
    for (const file of files.filter((f) => f.endsWith(".css"))) {
      for (const _ of readCode(file).matchAll(/!important/g)) {
        found.push(file);
      }
    }
    expect(found).toEqual(["style/app.css"]);
  });
});

describe("D4 letterbox invariant", () => {
  it("never imports xterm's FitAddon", () => {
    /** An import, a require, or a `loadAddon` of it — not a mention of it. */
    const use =
      /(?:import|require)[^\n]*addon-fit|new\s+FitAddon|loadAddon\([^)]*[Ff]it/;
    const offenders = files.filter(
      (file) => file !== SELF && use.test(readCode(file)),
    );
    /**
     * D4 turn 2: the browser letterboxes the host's fixed grid. `FitAddon`
     * exists to make a terminal claim its container's size, which is the exact
     * regression this decision was written to prevent — and the host would
     * revert it within one frame anyway.
     */
    expect(offenders).toEqual([]);
  });

  it("never scales the terminal with a CSS transform", () => {
    expect(readCode("style/main.css")).not.toMatch(/transform\s*:\s*scale/);
    /** Including from the one stylesheet whose whole subject is "the viewport
     * is too small", which is exactly where somebody would reach for it. */
    expect(readCode("style/narrow.css")).not.toMatch(/transform\s*:\s*scale/);
  });

  /**
   * `remote-control-eek.4`: a grid wider than its stage used to be clipped at
   * both edges by `overflow: hidden` + centring, silently. The stage scrolls
   * instead, and the centring is `margin: auto` on the letterbox rather than
   * `justify-content` on the stage — a centred flex item that overflows its
   * scroll container overflows past the scroll origin, so its leading edge can
   * never be reached. Both halves are asserted, because putting either back
   * restores the clipping.
   */
  it("scrolls an oversized grid instead of clipping it", () => {
    const main = readCode("style/main.css");
    const stage = /\.fd-stage\s*\{([^}]*)\}/.exec(main)?.[1] ?? "";
    const letterbox = /\.fd-letterbox\s*\{([^}]*)\}/.exec(main)?.[1] ?? "";
    expect(stage, ".fd-stage must exist in main.css").not.toBe("");
    expect(stage).toMatch(/overflow\s*:\s*auto/);
    expect(stage).not.toMatch(/overflow\s*:\s*hidden/);
    expect(stage).not.toMatch(/justify-content|align-items/);
    expect(letterbox).toMatch(/margin\s*:\s*auto/);
  });

  it("is not allowed to depend on @xterm/addon-fit", () => {
    const manifest = readFileSync(
      new URL("../../package.json", import.meta.url),
      "utf8",
    );
    expect(manifest).not.toContain("addon-fit");
  });
});
