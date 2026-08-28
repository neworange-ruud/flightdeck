import { describe, expect, it } from "vitest";
import { fixtureSeats } from "./fixture";
import { observers, viewerChipText, viewerChipTitle, webController } from "./seats";
import type { SeatInfo } from "./model";

/**
 * The viewer chip (2c/2f), and the one seat question with a wrong answer.
 */

const desktop: SeatInfo = {
  label: "desktop",
  address: null,
  browser: null,
  /** **Always** controlling: the desktop's keyboard is never revoked by a
   * browser taking over, which is why 2f gives it a transient strip and not a
   * dialog. */
  seat: "controlling",
  isDesktop: true,
  sinceLabel: "since launch",
};

function browser(label: string, seat: SeatInfo["seat"]): SeatInfo {
  return {
    label,
    address: "192.168.2.20",
    browser: "Chrome on macOS",
    seat,
    isDesktop: false,
    sinceLabel: "14 minutes",
  };
}

describe("finding the web controller", () => {
  it("does not mistake the desktop for the browser that took over", () => {
    /**
     * The trap: two rows report `Seat::Controlling` on a perfectly ordinary
     * connection. A naive search returns the desktop, and the evicted browser
     * is then told the *desktop* evicted it — which never happens and cannot be
     * acted on.
     */
    const seats = [desktop, browser("192.168.2.11", "controlling")];
    expect(seats.filter((s) => s.seat === "controlling")).toHaveLength(2);
    expect(webController(seats)?.label).toBe("192.168.2.11");
  });

  it("is null when no browser holds the seat", () => {
    /** A desktop plus N observers is legal and common. */
    expect(webController([desktop, browser("phone", "observing")])).toBeNull();
    expect(webController([desktop])).toBeNull();
    expect(webController([])).toBeNull();
  });

  it("lists observers without counting the desktop as one", () => {
    const seats = [desktop, browser("a", "observing"), browser("b", "observing")];
    expect(observers(seats).map((s) => s.label)).toEqual(["a", "b"]);
  });
});

describe("the chip's text", () => {
  it("names two seats rather than counting them (2f)", () => {
    expect(viewerChipText(fixtureSeats(), 2)).toBe("desktop + this tab");
  });

  it("grows by naming, which is what M3's viewer list will do", () => {
    const seats = [desktop, browser("this tab", "controlling"), browser("phone", "observing")];
    expect(viewerChipText(seats, 3)).toBe("desktop + this tab + phone");
  });

  it("falls back to a count only when the host sent no seats", () => {
    expect(viewerChipText([], 2)).toBe("2 viewers");
    expect(viewerChipText([], 1)).toBe("1 viewer");
  });

  it("puts each seat's role and age in the tooltip", () => {
    const title = viewerChipTitle([desktop, browser("phone", "observing")]);
    expect(title).toContain("desktop — controls input · since launch");
    expect(title).toContain("phone — read-only · 14 minutes");
  });
});
