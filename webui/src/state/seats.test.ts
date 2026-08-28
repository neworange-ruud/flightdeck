import { describe, expect, it } from "vitest";
import { fixtureSeats } from "./fixture";
import {
  inputHolder,
  observers,
  viewerChipText,
  viewerChipTitle,
  writers,
} from "./seats";
import type { SeatInfo } from "./model";

/**
 * The viewer chip (2c/2f), and the seat questions that used to have a wrong
 * answer.
 */

const desktop: SeatInfo = {
  label: "desktop",
  address: null,
  browser: null,
  /** **Always** a writer: the desktop's keyboard is never revoked by anything a
   * browser does, which is why 2f gives it a transient strip and not a dialog.
   * What it does not always have is the turn. */
  seat: "writing",
  holdsInput: false,
  isDesktop: true,
  isYou: false,
  sinceLabel: "since launch",
};

function browser(
  label: string,
  seat: SeatInfo["seat"],
  holdsInput = false,
  isYou = false,
): SeatInfo {
  return {
    label,
    /** Every browser here reports the same address on purpose: two tabs on one
     * laptop is the ordinary multi-viewer case, and it is the case any
     * address-matching shortcut gets wrong. */
    address: "192.168.2.20",
    browser: "Chrome on macOS",
    seat,
    holdsInput,
    isDesktop: false,
    isYou,
    sinceLabel: "14 minutes",
  };
}

/**
 * The reader's own tab **as the host actually describes it** — a real label,
 * plus the `is_you` mark that is the only thing allowed to turn it into
 * `this tab`.
 *
 * Deliberately never `browser("this tab", ...)`: a fixture that pre-baked the
 * answer into the label would let a chip which had stopped deriving it keep
 * passing, which is exactly how the chip came to disagree with D14.
 */
function thisTab(holdsInput = false): SeatInfo {
  return browser("192.168.2.20 · Chrome on macOS", "writing", holdsInput, true);
}

describe("finding who is typing", () => {
  it("does not mistake a seated writer for the one holding the turn", () => {
    /**
     * The trap v1 had, in its new shape. Three rows say `writing` on a
     * perfectly ordinary connection — that is what lifting the
     * single-controller restriction means — so "find the writer" finds three.
     * Only one of them is typing, and only `holdsInput` says which.
     */
    const seats = [
      desktop,
      browser("192.168.2.11", "writing", true),
      browser("192.168.2.12", "writing"),
    ];
    expect(writers(seats)).toHaveLength(3);
    expect(inputHolder(seats)?.label).toBe("192.168.2.11");
  });

  it("can name the desktop, because the rule is symmetric", () => {
    /** No surface has precedence: when the desktop is mid-burst it is the
     * holder, and the browser is the one being refused. A function that could
     * never return the desktop would be encoding a privilege the model
     * deliberately refuses. */
    const seats = [
      { ...desktop, holdsInput: true },
      browser("192.168.2.11", "writing"),
    ];
    expect(inputHolder(seats)?.isDesktop).toBe(true);
  });

  it("is null when the lock is free, which is a normal state", () => {
    /** Nobody has typed for `INPUT_LOCK_IDLE_MS`. Not an error, and not a
     * reason to invent a holder. */
    expect(inputHolder([desktop, browser("phone", "observing")])).toBeNull();
    expect(inputHolder([desktop])).toBeNull();
    expect(inputHolder([])).toBeNull();
  });

  it("lists observers without counting the desktop as one", () => {
    const seats = [
      desktop,
      browser("a", "observing"),
      browser("b", "observing"),
    ];
    expect(observers(seats).map((s) => s.label)).toEqual(["a", "b"]);
  });
});

describe("the chip's text", () => {
  it("names the seats that can type, and marks the one that is (2f)", () => {
    expect(viewerChipText(fixtureSeats(), 2)).toBe("desktop + this tab ✎");
  });

  it("marks nobody while the lock is free", () => {
    const seats = [desktop, thisTab()];
    expect(viewerChipText(seats, 2)).toBe("desktop + this tab");
  });

  it("grows by naming, which is what M3's viewer list will do", () => {
    const seats = [desktop, thisTab(true), browser("phone", "writing")];
    expect(viewerChipText(seats, 3)).toBe("desktop + this tab ✎ + phone");
  });

  it("counts observers rather than naming them, to keep the chip one line", () => {
    const seats = [
      desktop,
      thisTab(true),
      browser("phone", "observing"),
      browser("tv", "observing"),
    ];
    expect(viewerChipText(seats, 4)).toBe("desktop + this tab ✎ + 2 watching");
  });

  it("falls back to a count only when the host sent no seats", () => {
    expect(viewerChipText([], 2)).toBe("2 viewers");
    expect(viewerChipText([], 1)).toBe("1 viewer");
  });

  it("puts each seat's role, turn and age in the tooltip", () => {
    const title = viewerChipTitle([
      { ...desktop, holdsInput: true },
      thisTab(),
      browser("phone", "observing"),
    ]);
    expect(title).toContain("desktop — typing now · since launch");
    /** The tooltip has room for both, so the reader's row keeps its full label
     * *and* gains the marker. The compact chip above has room for one. */
    expect(title).toContain(
      "192.168.2.20 · Chrome on macOS · this tab — can type · 14 minutes",
    );
    expect(title).toContain("phone — read-only · 14 minutes");
  });
});

/**
 * `remote-control-eek.3`. D14 as revised says outright that "the browser's
 * viewer chip reads `desktop + this tab ✎`", and the chip did not: it rendered
 * the reader's own row by the host's label, so a real session read
 * `desktop + 192.168.2.20 · Chrome on macOS ✎`. The seat panel's rows mark the
 * reader from `is_you`; these pin the chip doing it from the same field, so the
 * two cannot describe the same seat differently.
 */
describe("the chip names the reader's own seat `this tab`", () => {
  it("says `this tab`, never the address the host labelled it with", () => {
    const seats = [desktop, thisTab(true)];
    expect(viewerChipText(seats, 2)).toBe("desktop + this tab ✎");
    /** The label is not merely shortened — it is not there at all. */
    expect(viewerChipText(seats, 2)).not.toContain("192.168.2.20");
    expect(viewerChipText(seats, 2)).not.toContain("Chrome on macOS");
  });

  it("marks exactly one of two tabs that share a machine", () => {
    /**
     * **The case `is_you` exists for, and the one any shortcut gets wrong.**
     * Two tabs on the same laptop send the same `User-Agent` from the same
     * address, so their rows are identical in every field a browser could
     * match on. Matching the address names both `this tab`; matching the label
     * does the same. Only the host can tell them apart, because it builds one
     * frame per recipient — and it says so in `is_you`.
     */
    const mine = thisTab(true);
    const theirs = browser("192.168.2.20 · Chrome on macOS", "writing");
    expect(mine.address).toBe(theirs.address);
    expect(mine.label).toBe(theirs.label);

    expect(viewerChipText([desktop, mine, theirs], 3)).toBe(
      "desktop + this tab ✎ + 192.168.2.20 · Chrome on macOS",
    );
    /** And the other way round, so neither ordering is accidentally right. */
    expect(viewerChipText([desktop, theirs, mine], 3)).toBe(
      "desktop + 192.168.2.20 · Chrome on macOS + this tab ✎",
    );
  });

  it("names an observing reader in the tooltip, which is where it fits", () => {
    /** An observer is not on the chip's one line — it cannot type, so it is
     * counted there — but it is still the reader, and the tooltip says so. */
    const watching = browser("192.168.2.20 · Chrome on macOS", "observing", false, true);
    expect(viewerChipText([desktop, watching], 2)).toBe("desktop + 1 watching");
    expect(viewerChipTitle([desktop, watching])).toContain(
      "192.168.2.20 · Chrome on macOS · this tab — read-only",
    );
  });

  it("marks nobody when the host marked nobody", () => {
    /** A host that sends no `is_you` on any row leaves every row named by its
     * label. Nothing is invented to fill the gap. */
    const seats = [desktop, browser("192.168.2.11", "writing", true)];
    expect(viewerChipText(seats, 2)).toBe("desktop + 192.168.2.11 ✎");
    expect(viewerChipTitle(seats)).not.toContain("this tab");
  });
});
