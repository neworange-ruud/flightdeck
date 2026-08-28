import { describe, expect, it } from "vitest";
import { reduce } from "./reducer";
import { createInitialState } from "./types";
import { NARROW_BELOW_PX, widthClass } from "./viewport";

/**
 * 1h's breakpoint, as arithmetic (`remote-control-eek.4`,
 * `specs/WEB_INTERFACE.md` §6.5 R17).
 *
 * This file exists because of what a media query cannot do here. `vitest` runs
 * in jsdom, which parses `@media (max-width: …)` and never evaluates it, so a
 * breakpoint that lived in CSS would be checked by nothing until the Playwright
 * job ran — and R6 registered that job **non-blocking until 2026-09-10**. The
 * decision is therefore a pure function of a number, and this is the test of
 * the number.
 *
 * What it proves: the boundary is where 1h puts it, in both directions, and
 * crossing it in either direction closes the slide-over. What it does **not**
 * prove is that any of it looks right — that is `narrowScreen.test.ts` for the
 * structure and `e2e/narrow.spec.ts` for the pixels.
 */

describe("1h's 900px boundary", () => {
  it("is the number the artboard states", () => {
    expect(NARROW_BELOW_PX).toBe(900);
  });

  it('reads "below 900px" as excluding 900 itself', () => {
    /** 1h says *below*. 900 across still fits 1a's 300px column beside a
     * terminal, so it gets the layout the artboards actually draw. */
    expect(widthClass(899)).toBe("narrow");
    expect(widthClass(900)).toBe("wide");
    expect(widthClass(901)).toBe("wide");
  });

  it("puts the devices this layout exists for on the narrow side", () => {
    /** A phone, a phone in landscape, and a portrait tablet — the acceptance
     * criterion's "tablet-sized viewport" is the 768 row. */
    expect(widthClass(390)).toBe("narrow");
    expect(widthClass(844)).toBe("narrow");
    expect(widthClass(768)).toBe("narrow");
    /** A landscape tablet and a laptop are wide, and get 1a unchanged. */
    expect(widthClass(1024)).toBe("wide");
    expect(widthClass(1600)).toBe("wide");
  });

  it("falls back to the drawn layout when the measurement is nonsense", () => {
    /** A detached iframe, a jsdom nobody configured, a `NaN` out of a
     * half-initialised host page. Falling back to the *derived* layout would
     * be the worse failure: wide is the one every artboard draws. */
    expect(widthClass(0)).toBe("wide");
    expect(widthClass(-1)).toBe("wide");
    expect(widthClass(Number.NaN)).toBe("wide");
    expect(widthClass(Number.POSITIVE_INFINITY)).toBe("wide");
  });
});

describe("viewport/measured", () => {
  it("stores the class, never the pixels", () => {
    const state = reduce(createInitialState(), {
      type: "viewport/measured",
      pixels: 500,
    });
    expect(state.width).toBe("narrow");
    /** No pixel count anywhere in the state: the layout is two values, and a
     * component that could read the raw width would eventually branch on it. */
    expect(Object.values(state)).not.toContain(500);
  });

  it("is identity when the class has not changed", () => {
    /** What makes an undebounced `resize` listener cheap: a drag across a
     * window edge re-renders twice, once per crossing, because the store only
     * notifies when `reduce` returns a different object. */
    const wide = reduce(createInitialState(), {
      type: "viewport/measured",
      pixels: 1400,
    });
    expect(
      reduce(wide, { type: "viewport/measured", pixels: 1200 }),
    ).toBe(wide);
  });

  it("closes the slide-over on every crossing, in both directions", () => {
    let state = reduce(createInitialState(), {
      type: "viewport/measured",
      pixels: 600,
    });
    state = reduce(state, { type: "sidebar/set", open: true });
    expect(state.sidebarOpen).toBe(true);

    /** Going wide, the sidebar is 1a's column again and an "open" flag would
     * be state nothing renders. */
    state = reduce(state, { type: "viewport/measured", pixels: 1200 });
    expect(state.width).toBe("wide");
    expect(state.sidebarOpen).toBe(false);

    /** And coming back, it stays closed: reopening a panel the reader
     * dismissed three resizes ago would be the app deciding for them. */
    state = reduce(state, { type: "viewport/measured", pixels: 600 });
    expect(state.sidebarOpen).toBe(false);
  });
});

describe("sidebar/set", () => {
  it("is refused at wide, so the flag cannot survive a crossing", () => {
    const wide = createInitialState();
    expect(wide.width).toBe("wide");
    /** Structural, not a convention: at wide there is no panel to open, so a
     * stray dispatch from a resize race cannot leave a flag the next crossing
     * would honour. */
    expect(reduce(wide, { type: "sidebar/set", open: true })).toBe(wide);
  });

  it("toggles at narrow, and is identity when nothing changes", () => {
    const narrow = reduce(createInitialState(), {
      type: "viewport/measured",
      pixels: 500,
    });
    const open = reduce(narrow, { type: "sidebar/set", open: true });
    expect(open.sidebarOpen).toBe(true);
    expect(reduce(open, { type: "sidebar/set", open: true })).toBe(open);
    expect(reduce(open, { type: "sidebar/set", open: false }).sidebarOpen).toBe(
      false,
    );
  });
});
