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
  sinceLabel: "since launch",
};

function browser(
  label: string,
  seat: SeatInfo["seat"],
  holdsInput = false,
): SeatInfo {
  return {
    label,
    address: "192.168.2.20",
    browser: "Chrome on macOS",
    seat,
    holdsInput,
    isDesktop: false,
    sinceLabel: "14 minutes",
  };
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
    const seats = [desktop, browser("this tab", "writing")];
    expect(viewerChipText(seats, 2)).toBe("desktop + this tab");
  });

  it("grows by naming, which is what M3's viewer list will do", () => {
    const seats = [
      desktop,
      browser("this tab", "writing", true),
      browser("phone", "writing"),
    ];
    expect(viewerChipText(seats, 3)).toBe("desktop + this tab ✎ + phone");
  });

  it("counts observers rather than naming them, to keep the chip one line", () => {
    const seats = [
      desktop,
      browser("this tab", "writing", true),
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
      browser("this tab", "writing"),
      browser("phone", "observing"),
    ]);
    expect(title).toContain("desktop — typing now · since launch");
    expect(title).toContain("this tab — can type · 14 minutes");
    expect(title).toContain("phone — read-only · 14 minutes");
  });
});
