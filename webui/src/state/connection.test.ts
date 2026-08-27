import { describe, expect, it } from "vitest";
import {
  ALL_CONNECTION_STATUSES,
  HELD_INPUT,
  QUEUED_INPUT,
  connectionStrip,
  hasControl,
  paneTone,
} from "./connection";
import { shouldRetry, versionMismatchBetween } from "./model";
import type { ShutdownReason } from "./model";
import { createInitialState } from "./types";
import type { AppState, ConnectionStatus } from "./types";

/**
 * Artboard 2c, row by row, plus 2d's five pane treatments — as pure state, so
 * the rules are asserted rather than eyeballed.
 */

function state(overrides: Partial<AppState> = {}): AppState {
  return { ...createInitialState(), ...overrides };
}

function connected(overrides: Partial<AppState> = {}): AppState {
  return state({
    connection: "connected",
    seat: "controlling",
    latencyMs: 18,
    ...overrides,
  });
}

describe("2c — every connection state", () => {
  it("connected: the ordinary hint row, an ok dot and the latency", () => {
    const strip = connectionStrip(connected());
    expect(strip.frame).toBe("neutral");
    /** `null` means "keep the mode's key hints" — 2c only replaces them when it
     * has something to say about the user's keystrokes. */
    expect(strip.message).toBeNull();
    expect(strip.status).toMatchObject({
      glyph: "dot",
      tone: "fd-tone-ok",
      text: "connected",
      detail: "18ms",
    });
    expect(strip.action).toBeNull();
  });

  it("connecting: names the address it is attaching to", () => {
    const strip = connectionStrip(
      state({ connection: "connecting", host: "192.168.2.14:7420" }),
    );
    expect(strip.status.glyph).toBe("spinner");
    expect(strip.status.text).toBe("connecting");
    expect(strip.status.detail).toBe("attaching to 192.168.2.14:7420");
  });

  it("connecting: says nothing about an address it does not know", () => {
    /** A fabricated address would be the first lie on a security-adjacent
     * screen, and this one is checkable by the user. */
    expect(connectionStrip(state({ connection: "connecting" })).status.detail).toBeNull();
  });

  it("reconnecting: §5.1's promise, the retry counter and the stale chip", () => {
    const strip = connectionStrip(
      state({
        connection: "reconnecting",
        retry: { attempt: 3, inSeconds: 4 },
        staleness: { frozenAt: "16:41:08", ago: "34s" },
      }),
    );
    expect(strip.frame).toBe("stale");
    expect(strip.message).toBe(HELD_INPUT);
    expect(strip.status.detail).toBe("attempt 3 · retry in 4s");
    expect(strip.staleChip).toBe("terminal stale 34s");
  });

  it("disconnected: says plainly that nothing will arrive, and offers r", () => {
    const strip = connectionStrip(
      state({ connection: "disconnected", retry: { attempt: 6, inSeconds: null } }),
    );
    expect(strip.frame).toBe("alert");
    expect(strip.message).toBe(
      "the host stopped answering · nothing you type will arrive",
    );
    expect(strip.status.detail).toBe("gave up after 6 attempts");
    expect(strip.action).toMatchObject({ key: "r", label: "Retry now", kind: "retry" });
  });

  it("catching up: input queues until the replay lands (§5.1)", () => {
    const strip = connectionStrip(
      state({
        connection: "catching_up",
        replay: {
          bytesDone: 25_000,
          bytesTotal: 41_984,
          fromByte: 1_204_992,
          truncated: false,
        },
      }),
    );
    expect(strip.message).toBe(QUEUED_INPUT);
    /** 2d prints the cursor grouped, because a bare seven-digit number is
     * unreadable and this one is meant to be read. */
    expect(strip.status.detail).toBe("replaying from byte 1 204 992");
  });

  it("version mismatch: a healthy connection with an old tab", () => {
    const strip = connectionStrip(
      connected({
        versionMismatch: { tabVersion: "v1.16.0", hostVersion: "v1.17.0" },
        latencyMs: 21,
      }),
    );
    /**
     * The whole point of this row: nothing about the link or about control is
     * wrong. 2c keeps `connected 21ms` and only changes the hue and the offer.
     */
    expect(strip.status.text).toBe("connected");
    expect(strip.status.detail).toBe("21ms");
    expect(strip.frame).toBe("info");
    expect(strip.message).toBe(
      "the host updated under you — this tab is running v1.16.0",
    );
    expect(strip.action).toMatchObject({
      key: "Enter",
      label: "Reload for v1.17.0",
      kind: "reload",
    });
  });

  it("version mismatch loses to a dead socket", () => {
    /** A stale tab on a dead connection is a dead connection first: the reload
     * cannot even be fetched. */
    const strip = connectionStrip(
      state({
        connection: "disconnected",
        versionMismatch: { tabVersion: "v1.16.0", hostVersion: "v1.17.0" },
      }),
    );
    expect(strip.frame).toBe("alert");
    expect(strip.action?.kind).toBe("retry");
  });

  it("revoked: amber, and the host is explicitly fine", () => {
    const strip = connectionStrip(state({ connection: "revoked" }));
    expect(strip.frame).toBe("stale");
    expect(strip.message).toBe(
      "access withdrawn from the desktop — the host is fine",
    );
    expect(strip.status.text).toBe("not allowed");
    expect(strip.action).toMatchObject({ label: "Enter a code", kind: "code" });
  });

  it("read-only: a real mode, and still a drained one", () => {
    const strip = connectionStrip(connected({ seat: "observing" }));
    /** D14 makes observation first-class, but an observer's input is answered
     * `Ignored`, so §5.1 still applies — and the strip explains why. */
    expect(strip.message).toContain("read-only");
    expect(hasControl(connected({ seat: "observing" }))).toBe(false);
  });

  it("covers every status in the union", () => {
    /** A new `ConnectionStatus` with no 2c row would otherwise fall through to
     * whatever the last branch happens to be. */
    for (const status of ALL_CONNECTION_STATUSES) {
      const strip = connectionStrip(state({ connection: status }));
      expect(strip.status.text.length).toBeGreaterThan(0);
    }
  });
});

describe("Q5 — Shutdown never shows reconnecting", () => {
  const reasons: readonly ShutdownReason[] = [
    "host_quit",
    "server_stopped",
    "token_revoked",
    "restarting",
    "unknown",
  ];

  it("only a restart is worth retrying — an unknown reason is not", () => {
    for (const reason of reasons) {
      expect(shouldRetry(reason)).toBe(reason === "restarting");
    }
  });

  it("host-initiated: names the host's own action, not a failure", () => {
    const strip = connectionStrip(
      state({
        connection: "stopped",
        shutdown: {
          reason: "host_quit",
          selfInitiated: false,
          detail: "6 agents were stopped",
          atLabel: "16:42",
        },
      }),
    );
    expect(strip.message).toBe(
      "FlightDeck was quit on the machine · 6 agents were stopped",
    );
    /** Not "disconnected", not "reconnecting": the host exited on purpose. */
    expect(strip.status.text).toBe("host exited cleanly");
    expect(strip.message).not.toContain("reconnect");
    expect(strip.action).toBeNull();
    expect(strip.note).toBe("start it again on the machine to reconnect");
  });

  it("self-initiated: acknowledges the user's own action", () => {
    const strip = connectionStrip(
      state({
        connection: "stopped",
        shutdown: {
          reason: "host_quit",
          selfInitiated: true,
          detail: "6 agents were stopped",
          atLabel: "16:42",
        },
      }),
    );
    /** 2c, verbatim. The difference between this and the row above is the whole
     * reason `self_initiated` is on the wire. */
    expect(strip.message).toBe(
      "you quit FlightDeck from this tab · 6 agents were stopped",
    );
  });

  it("distinguishes a stopped web interface from a quit FlightDeck", () => {
    const strip = connectionStrip(
      state({
        connection: "stopped",
        shutdown: {
          reason: "server_stopped",
          selfInitiated: false,
          detail: "",
          atLabel: "16:42",
        },
      }),
    );
    /** The agents are still alive and the desktop is still usable, which is a
     * materially different thing to be told. */
    expect(strip.message).toContain("FlightDeck is still running");
    expect(strip.status.text).toBe("web interface stopped");
  });

  it("keeps an unrecognised reason's own words rather than tidying them away", () => {
    const strip = connectionStrip(
      state({
        connection: "stopped",
        shutdown: {
          reason: "unknown",
          selfInitiated: false,
          detail: "listener closed by the supervisor",
          atLabel: "",
        },
      }),
    );
    expect(strip.message).toContain("listener closed by the supervisor");
    expect(strip.status.detail).toBeNull();
  });

  it("says so when the host gave no reason at all", () => {
    const strip = connectionStrip(
      state({
        connection: "stopped",
        shutdown: {
          reason: "unknown",
          selfInitiated: false,
          detail: "",
          atLabel: "16:42",
        },
      }),
    );
    expect(strip.message).toBe(
      "the host closed the connection and did not say why",
    );
  });
});

describe("2d — the five pane treatments", () => {
  it("live when connected with the terminal focused", () => {
    expect(paneTone(connected({ mode: "terminal" }))).toBe("live");
  });

  it("asleep in App mode — the picture is still true", () => {
    expect(paneTone(connected({ mode: "app" }))).toBe("asleep");
  });

  it("stale on every state where bytes have stopped arriving", () => {
    const frozen: readonly ConnectionStatus[] = [
      "reconnecting",
      "disconnected",
      "revoked",
      "stopped",
    ];
    for (const connection of frozen) {
      expect(paneTone(state({ connection, mode: "terminal" }))).toBe("stale");
    }
  });

  it("asleep-and-stale is a third state, not a coin toss", () => {
    expect(paneTone(state({ connection: "reconnecting", mode: "app" }))).toBe(
      "asleep_stale",
    );
  });

  it("staleness outranks catching up, which outranks asleep", () => {
    /** "What you are looking at is not true any more" is the only one of these
     * facts that can make a user act on a lie, so it wins. */
    expect(
      paneTone(state({ connection: "catching_up", mode: "app" })),
    ).toBe("catching_up");
    expect(paneTone(state({ connection: "reconnecting", mode: "app" }))).toBe(
      "asleep_stale",
    );
  });

  it("an access screen makes the frame behind it a photograph (2b)", () => {
    const withAccess = connected({
      mode: "terminal",
      access: {
        screen: "revoked",
        code: "",
        refused: "",
        attemptsRemaining: null,
        lockoutSeconds: null,
        revokedAgo: "12s ago",
      },
    });
    expect(paneTone(withAccess)).toBe("stale");
  });

  it("an eviction prompt is stale; choosing to watch is not (2f/D14)", () => {
    const evicted = connected({
      mode: "terminal",
      takeover: { kind: "evicted", byAddress: "192.168.2.11", lastInputAgo: "3s" },
    });
    expect(paneTone(evicted)).toBe("stale");
    /** Once the user picks `w`, the socket is still open and the bytes are
     * still arriving — and 2d's rule is that colour means live. */
    expect(paneTone({ ...evicted, takeover: null, seat: "observing" })).toBe("live");
  });

  it("connecting is not stale — there is nothing yet to be a photograph of", () => {
    expect(paneTone(state({ connection: "connecting", mode: "terminal" }))).toBe(
      "live",
    );
  });
});

describe("detecting a host that updated under an open tab", () => {
  it("compares the version this tab attached with against the host's now", () => {
    /**
     * No build stamp is needed and none exists: the SPA ships inside the binary
     * (D9), so the version this tab was built from *is* the `host_version` it
     * first attached with.
     */
    expect(versionMismatchBetween("v1.16.0", "v1.17.0")).toEqual({
      tabVersion: "v1.16.0",
      hostVersion: "v1.17.0",
    });
  });

  it("is null when nothing changed, or when we do not know", () => {
    expect(versionMismatchBetween("v1.16.0", "v1.16.0")).toBeNull();
    /** An empty string is "the host did not tell us", which is not a mismatch —
     * claiming one would send the user to reload for no reason. */
    expect(versionMismatchBetween("", "v1.17.0")).toBeNull();
    expect(versionMismatchBetween("v1.16.0", "")).toBeNull();
  });
});
