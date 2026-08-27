import type { ActivityEvent, ActivityTier } from "./model";
import { statusWord } from "./status";

/**
 * Artboard `2e — ACTIVITY FEED`, as pure functions.
 *
 * D11 makes this feed **the entire substitute for OS notifications** in the
 * browser — Web Push is structurally blocked under D1 — so the unread chip is
 * the only thing that will ever tell a user "something needs you". That is why
 * its precedence is mirrored from the host rather than re-derived here: two
 * implementations of "which of these matters most" would eventually disagree,
 * and the disagreement would be silent.
 *
 * The host's version is `src/web/activity.rs`'s `UnreadSummary` /
 * `tier_rank`, over `protocol::ActivityTier`. Same three tiers, same ranks,
 * same "first non-empty rank wins" rule.
 */

/**
 * **Lower is more urgent.** Byte-for-byte the host's `tier_rank`:
 * attention 0, finished 1, quiet 2.
 *
 * The host's own comment is worth carrying over, because it names the mistake
 * this function exists to avoid: this is *not* a reuse of the five-bucket
 * status rank the project dots use. That rank would put a still-working `quiet`
 * transition above a `finished` completion, which is exactly backwards for a
 * feed — a finished agent is news, an agent still working is not.
 */
export function tierRank(tier: ActivityTier): number {
  switch (tier) {
    case "attention":
      return 0;
    case "finished":
      return 1;
    case "quiet":
      return 2;
  }
}

/** Mirrors `activity::UnreadSummary`. */
export interface UnreadSummary {
  /** The most urgent tier with at least one unread event. */
  readonly tier: ActivityTier;
  /** Unread events at `tier`. */
  readonly countAtTier: number;
  /** Unread events across all tiers. */
  readonly totalUnread: number;
}

/**
 * `null` when there is nothing unread — an empty feed, or everything already
 * seen. The chip then shows the quiet "open me" affordance rather than
 * disappearing, because the feed has to stay reachable (2e: `a` opens it, and
 * the chip is the pointer-driven half of that).
 */
export function unreadSummary(
  events: readonly ActivityEvent[],
): UnreadSummary | null {
  /** Indexed by `tierRank`: [attention, finished, quiet] — the host's own
   * layout, so the two can be compared line by line. */
  const counts = [0, 0, 0];
  for (const event of events) {
    if (!event.read) {
      const rank = tierRank(event.tier);
      counts[rank] = (counts[rank] ?? 0) + 1;
    }
  }
  const totalUnread = counts.reduce((sum, n) => sum + n, 0);
  const rank = counts.findIndex((n) => n > 0);
  if (rank === -1) {
    return null;
  }
  const tier: ActivityTier =
    rank === 0 ? "attention" : rank === 1 ? "finished" : "quiet";
  return { tier, countAtTier: counts[rank] ?? 0, totalUnread };
}

/**
 * The status-bar chip, in its four renderings (2e's three tiers plus all-read).
 *
 * The words are the design's: **`▲ 2 need you`**, **`▲ 3 finished`**, and the
 * quiet `▵ activity` with its key hint. Note what the two loud tiers do *not*
 * say: they never say "2 events" or "2 notifications". They say what the events
 * are, because a count on its own is exactly the kind of chip a user learns to
 * ignore.
 *
 * `tone` is the tier, and `read` is the fourth value the CSS needs — the chip
 * "takes the colour of the most urgent unread event", so with nothing unread it
 * takes no colour at all.
 */
export interface UnreadChip {
  readonly text: string;
  readonly tone: ActivityTier | "read";
  /** The key that opens the feed, shown in the quiet rendering only (2e). */
  readonly key: string | null;
  /** The full unread breakdown, for the chip's `title` — nothing is lost when
   * the quiet rendering declines to print a number. */
  readonly title: string;
}

export function unreadChip(events: readonly ActivityEvent[]): UnreadChip {
  const summary = unreadSummary(events);
  if (summary === null) {
    return {
      text: "▵ activity",
      tone: "read",
      key: "a",
      title:
        events.length === 0
          ? "nothing has changed in 24 hours"
          : "everything here has been seen",
    };
  }
  const title = `${summary.totalUnread} unread · ${summary.countAtTier} ${summary.tier}`;
  switch (summary.tier) {
    /** Attention beats finished beats quiet, and red is the only one that
     * breathes (2e: once every 2.4s). */
    case "attention":
      return {
        text: `▲ ${summary.countAtTier} need you`,
        tone: "attention",
        key: null,
        title,
      };
    case "finished":
      return {
        text: `▲ ${summary.countAtTier} finished`,
        tone: "finished",
        key: null,
        title,
      };
    /** The quiet tier gets no count, per 2e's third row. The number is still
     * in the tooltip: quiet is not the same as hidden. */
    case "quiet":
      return { text: "▵ activity", tone: "quiet", key: "a", title };
  }
}

/**
 * A row's transition line: `in progress → waiting · asked a question`.
 *
 * Two things this must not do, both from §5.1:
 *
 *   - **Never collapse `unknown → unknown`.** It is the credible "we don't
 *     know", and printing one `unknown` (or worse, `idle`) would be the guess
 *     the requirement forbids.
 *   - **Never pad `reason`.** The host sends it empty when it has nothing
 *     honest to say, and an empty reason renders as nothing at all — no "—",
 *     no "status changed", no invented cause.
 */
export function transitionText(event: ActivityEvent): string {
  const arrow = `${statusWord(event.from)} → ${statusWord(event.to)}`;
  return event.reason === "" ? arrow : `${arrow} · ${event.reason}`;
}

/** 2e: the feed lists newest first, while the host backfills oldest first. */
export function newestFirst(
  events: readonly ActivityEvent[],
): readonly ActivityEvent[] {
  return [...events].reverse();
}

/**
 * D3, said out loud on every row. Selecting from the feed moves the *desktop's*
 * selection too, and 2e's judgement is that a hover title is the only warning
 * it needs, because selection is reversible and cheap.
 */
export const JUMP_HINT = "jump · also moves the desktop";
