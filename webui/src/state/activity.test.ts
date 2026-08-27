import { describe, expect, it } from "vitest";
import {
  newestFirst,
  tierRank,
  transitionText,
  unreadChip,
  unreadSummary,
} from "./activity";
import { fixtureActivity } from "./fixture";
import type { ActivityEvent, ActivityTier } from "./model";

/**
 * 2e's unread chip, and the one thing about it that has a wrong answer: the
 * precedence. This mirrors `src/web/activity.rs`'s `UnreadSummary` /
 * `tier_rank`, and these tests are written to fail if the two ever disagree.
 */

let seq = 0;
function event(
  tier: ActivityTier,
  read: boolean,
  overrides: Partial<ActivityEvent> = {},
): ActivityEvent {
  seq += 1;
  return {
    id: `e-${seq}`,
    atLabel: "now",
    projectId: "p",
    projectName: "project",
    sessionId: "s",
    sessionName: "session",
    from: "in_progress",
    to: tier === "attention" ? "waiting" : tier === "finished" ? "idle" : "unknown",
    reason: "",
    tier,
    read,
    ...overrides,
  };
}

describe("tier precedence — mirrored, not re-derived", () => {
  it("ranks the three tiers the way the host does: lower is more urgent", () => {
    expect(tierRank("attention")).toBe(0);
    expect(tierRank("finished")).toBe(1);
    expect(tierRank("quiet")).toBe(2);
  });

  it("attention beats finished", () => {
    const summary = unreadSummary([
      event("finished", false),
      event("finished", false),
      event("attention", false),
    ]);
    expect(summary).toEqual({ tier: "attention", countAtTier: 1, totalUnread: 3 });
  });

  it("finished beats quiet", () => {
    const summary = unreadSummary([
      event("quiet", false),
      event("quiet", false),
      event("finished", false),
    ]);
    expect(summary).toEqual({ tier: "finished", countAtTier: 1, totalUnread: 3 });
  });

  it("quiet is a tier, not an absence", () => {
    expect(unreadSummary([event("quiet", false)])).toEqual({
      tier: "quiet",
      countAtTier: 1,
      totalUnread: 1,
    });
  });

  it("does not let a still-working transition outrank a completion", () => {
    /**
     * The specific mistake the host's comment warns about: reusing the
     * five-bucket status rank the project dots use would put an in-progress
     * (`quiet`) event above a finished one, which is backwards for a feed. A
     * finished agent is news; one still working is not.
     */
    const summary = unreadSummary([
      event("quiet", false, { from: "idle", to: "in_progress" }),
      event("finished", false),
    ]);
    expect(summary?.tier).toBe("finished");
  });

  it("ignores read events entirely", () => {
    expect(
      unreadSummary([event("attention", true), event("finished", false)]),
    ).toMatchObject({ tier: "finished" });
  });

  it("is null for an empty feed and for an all-read one", () => {
    expect(unreadSummary([])).toBeNull();
    expect(unreadSummary([event("attention", true)])).toBeNull();
  });
});

describe("the chip's four renderings (2e)", () => {
  it("says what the events are, not how many notifications there are", () => {
    expect(unreadChip([event("attention", false), event("attention", false)])).toMatchObject(
      { text: "▲ 2 need you", tone: "attention" },
    );
    expect(
      unreadChip([event("finished", false), event("finished", false), event("finished", false)]),
    ).toMatchObject({ text: "▲ 3 finished", tone: "finished" });
  });

  it("the quiet tier prints no count, but keeps it in the tooltip", () => {
    const chip = unreadChip([event("quiet", false), event("quiet", false)]);
    expect(chip.text).toBe("▵ activity");
    expect(chip.tone).toBe("quiet");
    /** Quiet is not the same as hidden: the number is still recoverable. */
    expect(chip.title).toContain("2 unread");
  });

  it("all-read keeps the affordance and drops the claim", () => {
    const chip = unreadChip([event("attention", true)]);
    expect(chip.text).toBe("▵ activity");
    expect(chip.tone).toBe("read");
    /** The feed has to stay reachable — `a` is advertised on the chip itself. */
    expect(chip.key).toBe("a");
  });

  it("an empty feed says so honestly", () => {
    expect(unreadChip([]).title).toBe("nothing has changed in 24 hours");
  });

  it("takes the colour of the most urgent unread event", () => {
    /** One red event among ten green ones is still red. */
    const events = [
      ...Array.from({ length: 10 }, () => event("finished", false)),
      event("attention", false),
    ];
    expect(unreadChip(events).tone).toBe("attention");
  });
});

describe("§5.1 — unknown stays unknown", () => {
  it("renders both ends of the arrow, and the host's reason verbatim", () => {
    const row = fixtureActivity().find((e) => e.to === "unknown");
    expect(row).toBeDefined();
    /** Never one `unknown`, and never `idle`: this is the credible "we don't
     * know", and collapsing it would be the guess the requirement forbids. */
    expect(transitionText(row as ActivityEvent)).toBe(
      "unknown → unknown · Codex CLI reports no lifecycle",
    );
  });

  it("prints nothing at all when the host had nothing to say", () => {
    /** `reason` is empty when the host has nothing honest to say, and must
     * never be padded — no "—", no "status changed", no invented cause. */
    expect(transitionText(event("finished", false, { reason: "" }))).toBe(
      "in progress → idle",
    );
  });

  it("keeps the host's words when it did have some", () => {
    expect(
      transitionText(event("attention", false, { reason: "agent exited (code 1)" })),
    ).toBe("in progress → waiting · agent exited (code 1)");
  });
});

describe("ordering", () => {
  it("renders newest first while the host backfills oldest first", () => {
    const events = fixtureActivity();
    const shown = newestFirst(events);
    expect(shown[0]?.id).toBe(events[events.length - 1]?.id);
    expect(shown).toHaveLength(events.length);
  });

  it("does not mutate the host's array", () => {
    const events = fixtureActivity();
    newestFirst(events);
    expect(events[0]?.id).toBe("e-1");
  });
});
