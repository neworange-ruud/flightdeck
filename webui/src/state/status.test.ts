import { describe, expect, it } from "vitest";
import { fixtureSnapshot } from "./fixture";
import {
  projectGlyph,
  projectTone,
  sessionStatusText,
  statusGlyph,
  statusLabel,
  statusTone,
  statusWord,
} from "./status";
import type { Project, SessionStatus } from "./model";

const ALL: readonly SessionStatus[] = [
  "in_progress",
  "idle",
  "waiting",
  "error",
  "reviewing",
  "starting",
  "unknown",
];

const project = (name: string): Project => {
  const found = fixtureSnapshot().projects.find((p) => p.name === name);
  if (found === undefined) {
    throw new Error(`fixture has no project named ${name}`);
  }
  return found;
};

describe("status vocabulary (2g)", () => {
  it("labels every status, with no gaps", () => {
    for (const status of ALL) {
      expect(statusLabel(status)).not.toBe("");
    }
  });

  it("maps statuses onto the tokens 2g assigns them", () => {
    /** accent = interactive/in-flight, ok = idle/healthy, alert = attention. */
    expect(statusTone("in_progress")).toBe("accent");
    expect(statusTone("reviewing")).toBe("accent");
    expect(statusTone("idle")).toBe("ok");
    expect(statusTone("waiting")).toBe("alert");
    expect(statusTone("error")).toBe("alert");
  });

  it("never puts a status on the decoration tier", () => {
    /** 2g: if deleting it would lose a fact, it cannot be --fd-text-decor. A
     * status is the single most actionable fact on the screen. */
    for (const status of ALL) {
      expect(statusTone(status)).not.toBe("decor");
    }
  });

  it("puts the two 'we do not know' statuses on the lifted floor", () => {
    expect(statusTone("starting")).toBe("quiet");
    expect(statusTone("unknown")).toBe("quiet");
  });

  it("moves the glyph only while work is happening", () => {
    expect(statusGlyph("in_progress")).toBe("spinner");
    expect(statusGlyph("starting")).toBe("spinner");
    expect(statusGlyph("idle")).toBe("dot");
    expect(statusGlyph("waiting")).toBe("dot");
  });

  it("gives unknown a hollow glyph, never a filled one (§5.1)", () => {
    expect(statusGlyph("unknown")).toBe("hollow");
  });

  it("never guesses a status for an agent with no lifecycle", () => {
    const session = project("api-gateway").sessions.find(
      (s) => s.lifecycleNote !== null,
    );
    expect(session).toBeDefined();
    expect(sessionStatusText(session!)).toBe(
      "unknown → unknown · Codex CLI reports no lifecycle",
    );
    expect(sessionStatusText(session!)).not.toContain("idle");
  });

  it("drops the brackets for the truth under a manual override", () => {
    expect(statusWord("idle")).toBe("idle");
    expect(statusWord("in_progress")).toBe("in progress");
    expect(statusLabel("idle")).toBe("[idle]");
  });
});

describe("project dot precedence (2g / §5.1)", () => {
  /**
   * 1a is the fixed point: `flightdeck` (in progress + waiting + error) shows a
   * spinner, `api-gateway` (waiting) an alert dot, `web` (all idle) an ok dot.
   * So motion outranks attention, and attention outranks calm.
   */
  it("shows motion when any session is working", () => {
    expect(projectGlyph(project("flightdeck"))).toBe("spinner");
    expect(projectTone(project("flightdeck"))).toBe("alert");
  });

  it("shows attention when a session is waiting or errored", () => {
    expect(projectGlyph(project("api-gateway"))).toBe("dot");
    expect(projectTone(project("api-gateway"))).toBe("alert");
  });

  it("shows calm only when nothing needs anyone", () => {
    expect(projectGlyph(project("web"))).toBe("dot");
    expect(projectTone(project("web"))).toBe("ok");
  });

  it("does not claim health for a project of unknowns", () => {
    const unknownOnly: Project = {
      id: "p-x",
      name: "x",
      sessions: project("api-gateway").sessions.filter(
        (s) => s.status === "unknown",
      ),
    };
    expect(projectTone(unknownOnly)).toBe("quiet");
    expect(projectGlyph(unknownOnly)).toBe("hollow");
  });
});
