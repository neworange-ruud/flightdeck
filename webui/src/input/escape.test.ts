import { describe, expect, it } from "vitest";
import { ESC_ESC_WINDOW_MS, decideEscape } from "./escape";

/**
 * The 400 ms window from spec §5, tested as arithmetic rather than as a
 * keyboard. Both halves of the rule matter and both are graded: `Esc Esc`
 * inside the window leaves terminal focus, and a single `Esc` — or a second one
 * that arrives too late — still passes through to the agent.
 */
describe("decideEscape", () => {
  it("uses the 400 ms window the design specified", () => {
    expect(ESC_ESC_WINDOW_MS).toBe(400);
  });

  it("passes a lone Esc through to the agent and arms the window", () => {
    expect(decideEscape(null, 1_000)).toEqual({
      kind: "pass_through",
      armedAt: 1_000,
    });
  });

  it("leaves terminal focus on a second Esc inside the window", () => {
    expect(decideEscape(1_000, 1_399)).toEqual({ kind: "leave_focus" });
  });

  it("treats exactly 400 ms as inside the window", () => {
    /** "within 400ms": a user who lands on the boundary meant the chord. */
    expect(decideEscape(1_000, 1_400)).toEqual({ kind: "leave_focus" });
  });

  it("passes through, not focus-leaving, once the window has lapsed", () => {
    expect(decideEscape(1_000, 1_401)).toEqual({
      kind: "pass_through",
      armedAt: 1_401,
    });
  });

  it("re-arms from the late Esc, so the next one can still be a chord", () => {
    const late = decideEscape(1_000, 5_000);
    expect(late).toEqual({ kind: "pass_through", armedAt: 5_000 });
    expect(decideEscape(5_000, 5_200)).toEqual({ kind: "leave_focus" });
  });

  it("treats a backwards clock as a fresh press, not a chord", () => {
    /** A negative gap is not evidence of intent — a wall-clock jump should
     * not silently take the keyboard away from the agent. */
    expect(decideEscape(5_000, 4_900)).toEqual({
      kind: "pass_through",
      armedAt: 4_900,
    });
  });
});
