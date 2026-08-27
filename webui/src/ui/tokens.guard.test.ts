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
 * It also guards D4's `FitAddon` invariant, for the same reason: it is a rule
 * that a single well-meaning import would silently undo.
 */

const SRC = new URL("..", import.meta.url).pathname;

/** The palette's definitions live in exactly one file. */
const COLOUR_VALUE_EXEMPT = ["style/tokens.css"];
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
  });

  it("is not allowed to depend on @xterm/addon-fit", () => {
    const manifest = readFileSync(
      new URL("../../package.json", import.meta.url),
      "utf8",
    );
    expect(manifest).not.toContain("addon-fit");
  });
});
