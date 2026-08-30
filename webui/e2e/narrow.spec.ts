import type { Page } from "@playwright/test";

import {
  authenticate,
  describeHost,
  expect,
  renderedTerminal,
  test,
} from "./support/fixtures";

/**
 * The layout below 900px, in a real browser at a real tablet size
 * (`remote-control-eek.4`, `specs/WEB_INTERFACE.md` §6.5 R17).
 *
 * ## Why this file exists at all, given `src/ui/narrowScreen.test.ts`
 *
 * The unit suite proves everything about the narrow layout that can be proved
 * without a layout engine: the breakpoint, the state machine, the elements and
 * the attributes. It cannot prove a single one of the things this task is
 * actually *about*, because jsdom computes no boxes:
 *
 *   - that the git bar really is folded **into** the status bar rather than
 *     merely wrapped in a box with it;
 *   - that the sidebar really is over the terminal rather than beside it;
 *   - that the status bar's connection strip really did stay on the first
 *     line, which is 2c's rule 1;
 *   - and above all **that D4 still holds when the host's grid does not fit**:
 *     the terminal is not scaled, not refitted, not squeezed — it is the same
 *     number of pixels wide at 768 as it is at 1600, and the stage scrolls.
 *
 * That last one is the whole reason the issue called D4 "the interesting
 * constraint", so it is asserted by measuring the same element at two viewport
 * sizes and comparing. A `FitAddon`, a `transform: scale`, or a `width: 100%`
 * on the mount each fail it, and none of them can be seen from jsdom.
 *
 * Q6's flake policy applies here exactly as it does to `chain.spec.ts`; see
 * that file's quarantine block for the form.
 */

/** The acceptance criterion's words: "verified on a tablet-sized viewport". */
const TABLET = { width: 768, height: 1024 };
/** `playwright.config.ts`'s own viewport, where 1a fits comfortably. */
const DESKTOP = { width: 1600, height: 1000 };

/** A box, or a failure that says which selector had none. */
async function box(
  page: Page,
  selector: string,
): Promise<{ x: number; y: number; width: number; height: number }> {
  const found = await page.locator(selector).boundingBox();
  expect(found, `${selector} has no box`).not.toBeNull();
  return found!;
}

test.describe("FlightDeck Web below 900px", () => {
  test("folds the git bar into the status bar and slides the sidebar over", async ({
    page,
    host,
  }) => {
    await page.setViewportSize(TABLET);
    await authenticate(page, host);
    await expect
      .poll(async () => renderedTerminal(page), {
        message: `xterm.js never rendered the agent's banner.\n${describeHost(host)}`,
        timeout: 45_000,
      })
      .toContain("fake-agent: starting");

    const frame = page.locator(".fd-frame");
    await expect(frame).toHaveAttribute("data-width", "narrow");

    /* ── 1h: the git bar folds into the status bar ─────────────────────── */

    const gitbar = await box(page, ".fd-gitbar");
    const statusbar = await box(page, ".fd-statusbar");
    const footer = await box(page, ".fd-footer");
    /**
     * Folded: the status line is on top and the git line hangs directly below
     * it, both inside one box, with no gap between them for a second strip's
     * border to live in. `column-reverse` is what does it — see `narrow.css`
     * for why the status line takes the top (2c's frame colour is its
     * `border-top`, and it has to stay the top edge of the whole fold).
     */
    expect(statusbar.y).toBeLessThan(gitbar.y);
    /** Directly below, with no gap for a second strip's border to live in.
     * A 1px tolerance throughout, because these are subpixel layout values and
     * Q6's answer to flake is to not write it in the first place. */
    expect(Math.abs(gitbar.y - (statusbar.y + statusbar.height))).toBeLessThanOrEqual(1);
    expect(
      Math.abs(footer.height - (statusbar.height + gitbar.height)),
    ).toBeLessThanOrEqual(1);
    /** And it is genuinely at the bottom of the frame, not floating. */
    const frameBox = await box(page, ".fd-frame");
    expect(
      Math.abs(footer.y + footer.height - (frameBox.y + frameBox.height)),
    ).toBeLessThanOrEqual(1);

    /** D4's chip survives the fold and gains the narrow clause. */
    await expect(page.locator(".fd-gitbar .fd-geometry")).toContainText(
      "scroll, never scale",
    );

    /* ── 2c rule 1: the connection strip did not move ──────────────────── */

    const mode = await box(page, ".fd-mode");
    const conn = await box(page, ".fd-conn");
    const hints = await box(page, ".fd-statusbar__hints");
    /** Same line as the mode chip, pushed to the bar's right edge — in a
     * wrapping bar that is not automatic, which is why the hints were given a
     * line of their own instead of being left to wrap wherever. */
    expect(conn.y).toBeLessThan(mode.y + mode.height);
    expect(conn.x + conn.width).toBeLessThanOrEqual(statusbar.x + statusbar.width);
    expect(hints.y).toBeGreaterThanOrEqual(mode.y + mode.height - 1);
    /** 1h: the hints are still all there, on their own line. */
    await expect(page.locator(".fd-statusbar__hints")).toContainText(
      "command palette",
    );

    /* ── 1h: the sidebar is a slide-over, invoked from the session chip ── */

    await expect(page.locator(".fd-sidebar")).toBeHidden();
    const chip = page.locator(".fd-sessionchip");
    await expect(chip).toBeVisible();
    await chip.click();
    await expect(page.locator(".fd-sidebar")).toBeVisible();

    const sidebar = await box(page, ".fd-sidebar");
    const main = await box(page, ".fd-main");
    /**
     * *Over*, not beside: at wide the sidebar's right edge is the main pane's
     * left edge, and here the two overlap. That is the difference between a
     * slide-over and a narrower column, and it is invisible from jsdom.
     */
    expect(Math.abs(sidebar.x - frameBox.x)).toBeLessThanOrEqual(5);
    expect(sidebar.x + sidebar.width).toBeGreaterThan(main.x);
    /** 2e's rule, kept: some of the screen behind it stays visible, so the
     * reader can see the terminal is still there. */
    expect(sidebar.width).toBeLessThan(frameBox.width);

    /** Clicking outside puts it away — the pointer half of `Esc`. */
    await page.locator(".fd-mount").click();
    await expect(page.locator(".fd-sidebar")).toBeHidden();

    /** Nothing anywhere pushes the page itself sideways. */
    const overflow = await page.evaluate(
      () =>
        document.documentElement.scrollWidth - document.documentElement.clientWidth,
    );
    expect(overflow).toBeLessThanOrEqual(0);
  });

  test("still letterboxes the host's grid when it does not fit (D4)", async ({
    page,
    host,
  }) => {
    await page.setViewportSize(DESKTOP);
    await authenticate(page, host);
    await expect
      .poll(async () => renderedTerminal(page), {
        message: `xterm.js never rendered the agent's banner.\n${describeHost(host)}`,
        timeout: 45_000,
      })
      .toContain("fake-agent: starting");

    /** The host's grid, measured where it comfortably fits. */
    const wideMount = await box(page, ".fd-mount");
    const wideRows = await page.locator(".fd-mount .xterm-rows > div").count();
    expect(wideRows).toBeGreaterThan(0);
    /** Letterboxed at wide: dark margin either side, which is what D4 buys. */
    const wideStage = await box(page, ".fd-stage");
    expect(wideMount.width).toBeLessThan(wideStage.width);

    await page.setViewportSize(TABLET);
    await expect(page.locator(".fd-frame")).toHaveAttribute(
      "data-width",
      "narrow",
    );

    const narrowMount = await box(page, ".fd-mount");
    const narrowStage = await box(page, ".fd-stage");

    /**
     * **The assertion this file exists for.** The terminal is the same number
     * of pixels wide on a tablet as it was on a 1600px desktop: not scaled,
     * not refitted, not squeezed to the container. A `FitAddon` would have
     * re-gridded it, a `transform: scale` would have shrunk it, and a
     * `width: 100%` on the mount would have crushed it — each of those fails
     * here, and none of them fails a jsdom test.
     */
    expect(Math.abs(narrowMount.width - wideMount.width)).toBeLessThanOrEqual(1);
    expect(Math.abs(narrowMount.height - wideMount.height)).toBeLessThanOrEqual(1);
    /** Same grid, from the host, unchanged — the browser never asked. */
    await expect(page.locator(".fd-mount .xterm-rows > div")).toHaveCount(
      wideRows,
    );

    /**
     * And the overflow is **honest**: the stage scrolls rather than clipping
     * the grid at both edges, which is what `overflow: hidden` plus centring
     * used to do silently. Both edges are reachable, which is the part
     * `margin: auto` (rather than `justify-content: center`) buys — a centred
     * overflowing flex item overflows past the scroll origin and its leading
     * columns can never be scrolled to.
     */
    expect(narrowMount.width).toBeGreaterThan(narrowStage.width);
    const scroll = await page.evaluate(() => {
      const stage = document.querySelector(".fd-stage");
      if (stage === null) {
        return null;
      }
      const max = stage.scrollWidth - stage.clientWidth;
      stage.scrollLeft = max;
      const atEnd = Math.round(stage.scrollLeft);
      stage.scrollLeft = 0;
      const atStart = Math.round(stage.scrollLeft);
      return { max, atEnd, atStart };
    });
    expect(scroll).not.toBeNull();
    expect(scroll!.max).toBeGreaterThan(0);
    expect(scroll!.atEnd).toBe(Math.round(scroll!.max));
    expect(scroll!.atStart).toBe(0);

    /** The chip says so, in words, for the platforms whose scrollbars are
     * invisible until you touch them. */
    await expect(page.locator(".fd-geometry")).toContainText(
      "host owns geometry · scroll, never scale",
    );
  });
});

/**
 * A closed overlay must be *closed*: not laid out, not painted, and above all
 * not hit-testable.
 *
 * This test exists because the first version of `narrow.spec.ts` failed on a
 * real defect that no unit test in this repository could have seen. Playwright
 * reported it as a timeout clicking `.fd-mount`, with
 * `<aside hidden class="fd-feed" data-open="false"> subtree intercepts pointer
 * events` — the **closed** activity feed covering the right 470px of a 768px
 * viewport, so a tablet user could not click into the terminal at all: not to
 * focus it, not to dismiss the sidebar, not for anything.
 *
 * The cause was general, not local. `[hidden] { display: none }` is the UA
 * stylesheet's lowest-specificity rule and every overlay here sets
 * `display: flex` on a class, which beats it. Nine components defended against
 * that one selector at a time and five elements never got the rule at all
 * (`.fd-feed`, `.fd-tabs`, `.fd-pane`, `.fd-split`, `.fd-pane__banner`). The
 * fix is one document-level rule in `app.css`; `tokens.guard.test.ts` rule 6
 * keeps it, and this pins the behaviour it buys.
 *
 * jsdom does no hit-testing, so this assertion only exists here.
 */
test.describe("a closed overlay does not intercept (R17)", () => {
  /** Every element carrying the attribute must have no box at all. A
   * `display: none` element returns no client rects; anything the attribute
   * failed to close returns one. */
  async function nothingHiddenHasABox(page: Page): Promise<string[]> {
    return page.evaluate(() =>
      [...document.querySelectorAll("[hidden]")]
        .filter((el) => el.getClientRects().length > 0)
        .map((el) => el.className || el.tagName),
    );
  }

  /**
   * The same fact from the user's end: sample a grid of points over the
   * terminal stage and check that what the browser would deliver a click to is
   * never inside something that is `hidden`. This is the assertion that
   * describes the actual bug — "the element is there and it is eating the
   * click" — rather than a proxy for it.
   */
  async function ghostsOverTheTerminal(page: Page): Promise<string[]> {
    return page.evaluate(() => {
      const stage = document.querySelector(".fd-stage");
      if (stage === null) {
        return ["no .fd-stage"];
      }
      const rect = stage.getBoundingClientRect();
      const found: string[] = [];
      for (const fx of [0.08, 0.5, 0.92]) {
        for (const fy of [0.08, 0.5, 0.92]) {
          const x = rect.left + rect.width * fx;
          const y = rect.top + rect.height * fy;
          let el: Element | null = document.elementFromPoint(x, y);
          while (el !== null) {
            if (el.hasAttribute("hidden")) {
              found.push(
                `(${Math.round(x)},${Math.round(y)}) intercepted by .${el.className}`,
              );
              break;
            }
            el = el.parentElement;
          }
        }
      }
      return found;
    });
  }

  test("at 768px, with every overlay closed and after each has been opened and closed", async ({
    page,
    host,
  }) => {
    await page.setViewportSize(TABLET);
    await authenticate(page, host);
    await expect
      .poll(async () => renderedTerminal(page), {
        message: `xterm.js never rendered the agent's banner.\n${describeHost(host)}`,
        timeout: 45_000,
      })
      .toContain("fake-agent: starting");
    await expect(page.locator(".fd-frame")).toHaveAttribute(
      "data-width",
      "narrow",
    );

    /**
     * The resting state first. This covers the overlays a browser cannot open
     * on its own — `.fd-dialog` needs the host to publish one (D13),
     * `.fd-takeover` needs a contested seat and `.fd-access` needs the
     * credential to be refused — all of which are closed here and all of which
     * would show up as a box if the attribute were not doing its job. It also
     * covers `.fd-pane__banner`, which is `hidden` in the `live` tone and was
     * one of the five: an absolutely-positioned, near-opaque strip across the
     * bottom of every live terminal.
     */
    expect(await nothingHiddenHasABox(page)).toEqual([]);
    expect(await ghostsOverTheTerminal(page)).toEqual([]);

    /**
     * And now each overlay this task touched, opened and closed, with the
     * invariant re-checked after every close — because "closed" is a state
     * that is only ever reached by going through "open", and the feed's bug
     * was present from the first paint precisely because nobody ever checked
     * the resting state either.
     *
     * `.fd-info` is one element for all three of `ll5.8`'s read-only panels
     * (`AppState.readOnly` holds one at a time), so driving help drives the
     * root that matters.
     */
    const overlays = [
      { name: "activity feed (2e)", root: ".fd-feed", key: "a" },
      { name: "slide-over sidebar (1h)", root: ".fd-sidebar", key: "s" },
      { name: "command palette (1d)", root: ".fd-palette", key: "Control+g" },
      { name: "help panel (R16)", root: ".fd-info", key: "?" },
    ];

    const frame = page.locator(".fd-frame");

    for (const overlay of overlays) {
      /**
       * Back to App mode between iterations, because these are all App-mode
       * keys and the previous pass ended by clicking the terminal.
       *
       * It has to be a click on a **button**, and 1h's session chip is the one
       * this width offers. Clicking the logo band would release the keys just
       * as well — 1a advertises exactly that — but the band holds nothing
       * focusable, so focus would land on `document.body`, which is an
       * *ancestor* of `.fd-frame`: a keydown there bubbles to `html` and never
       * reaches the frame's listener. Clicking the chip leaves focus inside
       * the frame, and its `Esc` closes the sidebar it just opened. No 400 ms
       * `Esc Esc` window is involved, so nothing here can race.
       */
      await page.locator(".fd-sessionchip").click();
      await page.keyboard.press("Escape");
      await expect(frame).toHaveAttribute("data-mode", "app");

      await page.keyboard.press(overlay.key);
      await expect(page.locator(overlay.root), overlay.name).toBeVisible();

      await page.keyboard.press("Escape");
      await expect(page.locator(overlay.root), overlay.name).toBeHidden();

      expect(await nothingHiddenHasABox(page), overlay.name).toEqual([]);
      expect(await ghostsOverTheTerminal(page), overlay.name).toEqual([]);

      /**
       * The functional half, and the one that actually failed: the terminal
       * takes a click. A `timeout` well under Playwright's default so a
       * regression reports in seconds rather than in half a minute of
       * "element is visible, enabled and stable".
       */
      await page.locator(".fd-mount").click({ timeout: 10_000 });
      /** `data-mode` rather than the mode chip's text: the chip drains to
       * `MODE: —` whenever this tab does not hold the input lock (§5.1), which
       * is a fact about the seat and not about whether the click landed. */
      await expect(frame).toHaveAttribute("data-mode", "terminal");
    }
  });
});
