import { describe, expect, it } from "vitest";

import { gitOf, seatOf, sessionOf, snapshotFromWire, statusOf } from "./adapt";
import { decodeBase64, encodeBase64 } from "./frames";
import type {
  WireCommandView,
  WireGitBar,
  WireSeatInfo,
  WireSessionView,
  WireSnapshot,
  WireTerminalView,
} from "./frames";

/**
 * The wire → model mapping (`remote-control-hgqy`'s open decision), and the
 * base64 both directions of the byte path depend on.
 *
 * These are the two places the browser could quietly start *inferring* facts
 * the host did not send, which is what §5.1's "unknown stays unknown" forbids.
 */

const git: WireGitBar = {
  branch: "flightdeck/fix-login",
  added: 3,
  modified: 2,
  removed: 1,
  ahead: 3,
  behind: 0,
  drift: 4,
  has_upstream: true,
  files_changed: 6,
  collected: true,
};

const terminal: WireTerminalView = {
  terminal_id: "tab-1:primary",
  session_id: "tab-1",
  role: "primary",
  title: "agent",
  geometry: { cols: 120, rows: 34 },
  byte_len: 10,
  replay_from: 0,
  alive: true,
};

const session: WireSessionView = {
  session_id: "tab-1",
  project_id: "/repo",
  name: "fix-login",
  agent: "claude",
  agent_display_name: "Claude Code",
  phase: "ready",
  status: {
    interpreted: "working",
    manual: null,
    bucket: "in_progress",
    running_time_secs: 12,
  },
  git,
  terminals: [terminal],
  lifecycle_reporting: true,
};

describe("the three-way git union", () => {
  it("reads git that has not answered as unknown, never as clean", () => {
    expect(gitOf({ ...git, collected: false }, false)).toEqual({
      kind: "unknown",
    });
    /** The trap: a not-yet-collected bar is all zeros, which *looks* clean. */
    expect(
      gitOf(
        {
          ...git,
          collected: false,
          added: 0,
          modified: 0,
          removed: 0,
        },
        false,
      ),
    ).toEqual({ kind: "unknown" });
  });

  it("distinguishes no_upstream from known", () => {
    expect(gitOf({ ...git, has_upstream: false }, false)).toEqual({
      kind: "no_upstream",
    });
    expect(gitOf(git, true)).toEqual({
      kind: "known",
      dirty: true,
      added: 3,
      removed: 1,
      drift: 4,
      recovered: true,
    });
  });

  it("is clean only when the host collected zero changes", () => {
    const clean = gitOf(
      { ...git, added: 0, modified: 0, removed: 0 },
      false,
    );
    expect(clean).toMatchObject({ kind: "known", dirty: false });
  });
});

describe("status", () => {
  it("prefers the interpreted label over the bucket, which cannot tell starting from unknown", () => {
    expect(
      statusOf({
        interpreted: "starting",
        manual: null,
        bucket: "unknown",
        running_time_secs: 0,
      }),
    ).toEqual({ status: "starting", manual: false, observed: null });
  });

  it("keeps the observed status beside a hand-set one", () => {
    expect(
      statusOf({
        interpreted: "idle",
        manual: "blocked",
        bucket: "waiting",
        running_time_secs: 0,
      }),
    ).toEqual({ status: "waiting", manual: true, observed: "idle" });
  });

  it("falls back to the bucket for a label it has never heard of", () => {
    expect(
      statusOf({
        interpreted: "brand new label",
        manual: null,
        bucket: "waiting",
        running_time_secs: 0,
      }).status,
    ).toBe("waiting");
  });
});

describe("session", () => {
  it("reports the absence of lifecycle hooks as data, naming the agent", () => {
    const mapped = sessionOf({ ...session, lifecycle_reporting: false }, "main");
    expect(mapped.lifecycleNote).toBe("Claude Code reports no lifecycle");
    /** And the presence of hooks adds no note at all. */
    expect(sessionOf(session, "main").lifecycleNote).toBeNull();
  });

  it("renders a session with no agent process yet as starting, not as a status", () => {
    const mapped = sessionOf({ ...session, phase: "creating" }, "main");
    expect(mapped.startingNote).toBe("creating worktree…");
  });

  it("maps the primary terminal to the agent tab and keeps the wire id", () => {
    const mapped = sessionOf(session, "main");
    expect(mapped.terminals).toEqual([
      { id: "tab-1:primary", title: "agent", kind: "agent" },
    ]);
  });

  it("drops the git bar rather than drawing one from an unanswered git", () => {
    expect(
      sessionOf({ ...session, git: { ...git, collected: false } }, "main")
        .gitBar,
    ).toBeNull();
    expect(sessionOf(session, "main").gitBar).toMatchObject({
      branch: "flightdeck/fix-login",
      base: "main",
      baseAhead: 4,
    });
  });
});

describe("snapshot", () => {
  const wire: WireSnapshot = {
    type: "snapshot",
    protocol_version: 2,
    host_version: "1.16.0",
    server_time_ms: 1_000_000,
    viewer_id: "v1",
    seat: "writing",
    seats: [
      {
        viewer_id: null,
        label: "desktop",
        seat: "writing",
        since_ms: 940_000,
        is_you: false,
      },
      {
        viewer_id: "v1",
        label: "127.0.0.1 · Chrome",
        seat: "writing",
        holds_input: true,
        since_ms: 999_000,
        is_you: true,
      },
    ],
    last_input_seq: 0,
    projects: [
      {
        project_id: "/repo",
        name: "flightdeck",
        root: "/repo",
        base_branch: "main",
        sessions: [session],
      },
    ],
    selection: {
      project_id: "/repo",
      session_id: "tab-1",
      terminal_id: "tab-1:primary",
    },
    geometry: { cols: 120, rows: 34 },
    replay_capacity_bytes: 262_144,
    activity: [],
  };

  it("carries the host's geometry through untouched (D4)", () => {
    expect(snapshotFromWire(wire).geometry).toEqual({ cols: 120, rows: 34 });
  });

  it("marks the desktop seat by its absent viewer id", () => {
    const seats = snapshotFromWire(wire).seats;
    expect(seats.map((s) => s.isDesktop)).toEqual([true, false]);
    expect(seats[1]?.sinceLabel).toBe("just now");
  });

  /**
   * `remote-control-ll5.9`: artboard 2f's three facts must not depend on which
   * frame delivered the seat. A snapshot and a `Delta::Seats` carry the same
   * `WireSeatInfo` shape and the same `server_time_ms` beside it, and both go
   * through `seatOf` — this pins that there is only one mapping, because two of
   * them is exactly how one path came to drop the `connected` fact.
   */
  it("maps a seat identically whether it arrived in a snapshot or a delta", () => {
    const row: WireSeatInfo = {
      viewer_id: "v2",
      label: "192.168.2.20 · Safari on iOS",
      address: "192.168.2.20",
      user_agent_label: "Safari on iOS",
      seat: "writing",
      holds_input: true,
      since_ms: 988_000,
      is_you: false,
    };

    const fromSnapshot = snapshotFromWire({ ...wire, seats: [row] }).seats[0];
    const fromDelta = seatOf(row, wire.server_time_ms);

    expect(fromDelta).toEqual(fromSnapshot);
    expect(fromDelta).toEqual({
      label: "192.168.2.20 · Safari on iOS",
      address: "192.168.2.20",
      browser: "Safari on iOS",
      seat: "writing",
      holdsInput: true,
      isDesktop: false,
      isYou: false,
      sinceLabel: "12s ago",
    });
  });

  it("reads an absent holds_input as `not this row`, never as `free`", () => {
    /**
     * The additive-field rule, applied to the one field where the wrong default
     * would be a claim rather than a gap. `false` on every row of an older
     * host's list is true of every row of an older host's list — it says
     * nothing about whether a lock is held, which is exactly right, because
     * such a host has no lock.
     */
    const row: WireSeatInfo = {
      viewer_id: "v2",
      label: "192.168.2.20",
      seat: "writing",
      since_ms: 988_000,
      is_you: false,
    };
    expect(seatOf(row, wire.server_time_ms).holdsInput).toBe(false);
  });

  it("undates a seat the host sent no clock for, and nothing else", () => {
    /** The one difference a delta can have: an older host sends no
     * `server_time_ms`. The row loses its date and keeps everything else —
     * never a fabricated duration, and never a negative one. */
    const row: WireSeatInfo = {
      viewer_id: "v2",
      label: "192.168.2.20 · Safari on iOS",
      address: "192.168.2.20",
      user_agent_label: "Safari on iOS",
      seat: "writing",
      since_ms: 988_000,
      is_you: false,
    };
    expect(seatOf(row, null)).toEqual({
      ...seatOf(row, wire.server_time_ms),
      sinceLabel: "",
    });
  });

  it("falls back to the first session when the host reports no selection", () => {
    const mapped = snapshotFromWire({ ...wire, selection: {} });
    expect(mapped.selection).toEqual({
      projectId: "/repo",
      sessionId: "tab-1",
      terminalId: "tab-1:primary",
    });
  });

  it("survives a host with nothing open", () => {
    const mapped = snapshotFromWire({ ...wire, projects: [], selection: {} });
    expect(mapped.projects).toEqual([]);
    expect(mapped.selection).toEqual({
      projectId: "",
      sessionId: "",
      terminalId: "",
    });
  });

  /** `remote-control-ll5.7`: D3/D8's split-view flag, carried on
   * `Selection` rather than invented locally. */
  describe("split_view", () => {
    it("maps a true flag through", () => {
      const mapped = snapshotFromWire({
        ...wire,
        selection: { ...wire.selection, split_view: true },
      });
      expect(mapped.splitView).toBe(true);
    });

    it("maps a false flag through", () => {
      const mapped = snapshotFromWire({
        ...wire,
        selection: { ...wire.selection, split_view: false },
      });
      expect(mapped.splitView).toBe(false);
    });

    it("defaults to false rather than guessing when the host omits it", () => {
      const mapped = snapshotFromWire({ ...wire, selection: {} });
      expect(mapped.splitView).toBe(false);
    });
  });

  /**
   * `remote-control-ll5.12`: the palette's whole inventory rides on the
   * snapshot, so the rename below is the only thing standing between
   * `src/web/commands.rs`'s table and what the user reads.
   */
  describe("commands", () => {
    const row: WireCommandView = {
      id: "abandon_worktree",
      label: "Abandon Worktree",
      group: "Worktree",
      run: { name: "abandon_worktree" },
      annotation: "destructive",
      refusal: "Abandoning a worktree discards work. Confirm it from the desktop.",
    };

    it("renames every field and defaults nothing into existence", () => {
      const mapped = snapshotFromWire({ ...wire, commands: [row] });
      expect(mapped.commands).toEqual([
        {
          id: "abandon_worktree",
          label: "Abandon Worktree",
          group: "Worktree",
          run: { name: "abandon_worktree" },
          hostOnly: false,
          answersDialog: false,
          annotation: "destructive",
          target: null,
          refusal:
            "Abandoning a worktree discards work. Confirm it from the desktop.",
        },
      ]);
    });

    it("carries the host's flags through (D16, D13)", () => {
      const mapped = snapshotFromWire({
        ...wire,
        commands: [
          { ...row, host_only: true },
          { ...row, id: "dialog_confirm", answers_dialog: true },
        ],
      });
      expect(mapped.commands[0]?.hostOnly).toBe(true);
      expect(mapped.commands[1]?.answersDialog).toBe(true);
    });

    it("maps the four target kinds by name", () => {
      const targets = ["project", "session", "terminal", "unread_activity"];
      const mapped = snapshotFromWire({
        ...wire,
        commands: targets.map((target) => ({ ...row, target })),
      });
      expect(mapped.commands.map((c) => c.target)).toEqual(targets);
    });

    /** The host's own `#[serde(other)]` arm reaching the browser. Kept as
     * `unrecognized` rather than flattened to `null`, because the difference
     * decides whether the row is skipped or sent with no argument. */
    it("keeps a target kind it does not know as unrecognized, not null", () => {
      const mapped = snapshotFromWire({
        ...wire,
        commands: [{ ...row, target: "pod" }],
      });
      expect(mapped.commands[0]?.target).toBe("unrecognized");
    });

    /** No local fallback, by design: a browser cannot know what a host that
     * says nothing is able to run. */
    it("is empty when the host sends no inventory", () => {
      expect(snapshotFromWire(wire).commands).toEqual([]);
      expect(snapshotFromWire({ ...wire, commands: [] }).commands).toEqual([]);
    });
  });
});

describe("base64", () => {
  it("round-trips bytes that are not text", () => {
    const bytes = new Uint8Array([0xde, 0xad, 0xbe, 0xef]);
    expect(encodeBase64(bytes)).toBe("3q2+7w==");
    expect([...decodeBase64("3q2+7w==")]).toEqual([...bytes]);
  });

  it("encodes a keystroke the way protocol.rs does", () => {
    /** `input_carries_a_seq_the_ack_answers` in src/web/protocol/tests.rs. */
    expect(encodeBase64(new TextEncoder().encode("ls\r"))).toBe("bHMN");
  });

  it("keeps a split UTF-8 sequence intact as bytes", () => {
    /** `é` is 0xc3 0xa9; if either half went through a JS string it would come
     * out as U+FFFD. The decoder is xterm's, and it needs the raw pair. */
    const first = decodeBase64(encodeBase64(new Uint8Array([0xc3])));
    const second = decodeBase64(encodeBase64(new Uint8Array([0xa9])));
    expect([...first, ...second]).toEqual([0xc3, 0xa9]);
  });
});
