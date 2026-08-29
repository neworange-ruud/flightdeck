import type { ShutdownState, StatusGlyph } from "./model";
import type { AppState, ConnectionStatus } from "./types";

/**
 * Artboard `2c — CONNECTION STATES` and `2d — ASLEEP vs STALE`, as pure
 * functions of state.
 *
 * Everything here is a string or a token *name*, never a colour and never a
 * DOM node, for the same reason `src/state/status.ts` is: the three rules 2c
 * states are rules about *behaviour*, and behaviour that lives in a pure
 * function is a unit test rather than a screenshot.
 *
 * The three rules, quoted:
 *
 *   1. **The position never moves**, so a glance always lands on it. Enforced
 *      by the status bar's structure (the flexible spacer always sits
 *      immediately before `.fd-conn`), and asserted in
 *      `src/ui/connectionStates.test.ts`.
 *   2. **Anything that costs the user control drains the mode chip**, because
 *      the mode is a lie while input is not arriving — see `modeChip` in
 *      `src/ui/statusBar.ts`.
 *   3. **The whole bar takes the state's frame colour**, which is the only
 *      chrome in the app that ever changes hue — `frame` below.
 */

/**
 * Which hue the whole bar takes. A token *meaning*, resolved to a colour by
 * `src/style/states.css`:
 *
 * `neutral` the ordinary frame rule · `stale` amber, "still true a moment ago"
 * · `alert` red, the host stopped answering · `info` blue, reload to update ·
 * `stopped` the separator grey of a host that exited on purpose.
 */
export type StripFrame = "neutral" | "stale" | "alert" | "info" | "stopped";

/** The one keyed button 2c puts at the right-hand end of the bar. */
export interface StripAction {
  /** The key that performs it, shown in its own box (`r`, `Enter`). */
  readonly key: string;
  readonly label: string;
  /** What the caller should do; the component never decides. */
  readonly kind: "retry" | "reload" | "code";
  /** The hue the button takes — the same vocabulary as `frame`. */
  readonly tone: StripFrame;
}

export interface ConnectionStrip {
  readonly frame: StripFrame;
  /**
   * The sentence next to the mode chip, or `null` to keep the mode's ordinary
   * key hints (2c's `connected` and `connecting` rows keep them).
   */
  readonly message: string | null;
  /** The `● connected 18ms` group. `detail` is the quieter second half. */
  readonly status: {
    readonly glyph: StatusGlyph;
    readonly tone: string;
    readonly text: string;
    readonly detail: string | null;
  };
  readonly action: StripAction | null;
  /** 2c's amber `terminal stale 34s` chip, present only while it is true. */
  readonly staleChip: string | null;
  /** 2c's trailing muted sentence on the host-quit row. */
  readonly note: string | null;
}

/**
 * §5.1's two phrases, in one place so they cannot be invented twice.
 *
 * They are load-bearing, not decorative: a user typing into a stale terminal is
 * entitled to know that the keystrokes are being kept rather than eaten, and
 * these are the exact words the design chose to promise it.
 */
export const HELD_INPUT = "keystrokes are being held";
export const QUEUED_INPUT = "input queues until the replay lands";

/**
 * The read-only message. D14 makes observation a real mode, so this is a
 * statement of a chosen posture, not an error — but it *is* a control-losing
 * state, so it drains the chip like the others (§5.1).
 */
export const READ_ONLY = "read-only · your keystrokes are not being sent";

/**
 * The whole 2c row for the state we are in.
 *
 * The order of the branches is the order of precedence, and it is a claim about
 * which fact matters most when several are true at once:
 *
 *   - **`versionMismatch` is checked last of the "wrong" things**, because 2c
 *     draws it *on top of a healthy connection* — the link is fine and control
 *     was never lost, the tab is merely old. A stale tab on a dead socket is a
 *     dead socket first.
 *   - **`shutdown` outranks every transport state**, because Q5's entire point
 *     is that a host which said goodbye must never be described as coming back.
 *   - **`revoked` outranks `shutdown`**, because it is the one shutdown that is
 *     not about the host at all. The host closed the socket, so a `Shutdown`
 *     frame is recorded (§6.5 R20) — but drawing the stopped row for it would
 *     put `FLIGHTDECK STOPPED` on a machine that is running perfectly well and
 *     offer "start it again on the machine" to a user whose only problem is a
 *     credential. 2c gives this its own row for exactly that reason.
 *   - **`seat` is checked while connected**, since read-only is the one loss of
 *     control that happens with a perfectly good connection.
 */
export function connectionStrip(state: AppState): ConnectionStrip {
  if (state.connection === "revoked") {
    return {
      frame: "stale",
      message: "access withdrawn from the desktop — the host is fine",
      status: {
        glyph: "dot",
        tone: "fd-tone-stale",
        text: "not allowed",
        detail: null,
      },
      action: {
        key: "Enter",
        label: "Enter a code",
        kind: "code",
        tone: "stale",
      },
      staleChip: staleChipFor(state),
      note: null,
    };
  }
  if (state.connection === "stopped" || state.shutdown !== null) {
    return stoppedStrip(state);
  }
  if (state.connection === "disconnected") {
    const attempts = state.retry?.attempt ?? null;
    return {
      frame: "alert",
      message: "the host stopped answering · nothing you type will arrive",
      status: {
        glyph: "dot",
        tone: "fd-tone-alert",
        text: "disconnected",
        detail: attempts === null ? null : `gave up after ${attempts} attempts`,
      },
      action: { key: "r", label: "Retry now", kind: "retry", tone: "alert" },
      staleChip: staleChipFor(state),
      note: null,
    };
  }
  if (state.connection === "reconnecting") {
    return {
      frame: "stale",
      /** §5.1: say where the keys are going while they are going nowhere. */
      message: HELD_INPUT,
      status: {
        glyph: "spinner",
        tone: "fd-tone-stale",
        text: "reconnecting",
        detail: retryDetail(state),
      },
      action: null,
      staleChip: staleChipFor(state),
      note: null,
    };
  }
  if (state.connection === "catching_up") {
    return {
      frame: "neutral",
      message: QUEUED_INPUT,
      status: {
        glyph: "spinner",
        tone: "fd-tone-accent",
        text: "catching up",
        detail:
          state.replay === null
            ? null
            : `replaying from byte ${groupDigits(state.replay.fromByte)}`,
      },
      action: null,
      staleChip: null,
      note: null,
    };
  }
  if (state.connection === "connecting") {
    return {
      frame: "neutral",
      message: null,
      status: {
        glyph: "spinner",
        tone: "fd-tone-accent",
        text: "connecting",
        detail: state.host === "" ? null : `attaching to ${state.host}`,
      },
      action: null,
      staleChip: null,
      note: null,
    };
  }

  /* Connected. Two things can still be true on a healthy link. */
  const latency = state.latencyMs === null ? null : `${state.latencyMs}ms`;
  if (state.versionMismatch !== null) {
    const { tabVersion, hostVersion } = state.versionMismatch;
    return {
      frame: "info",
      message: `the host updated under you — this tab is running ${tabVersion}`,
      status: {
        glyph: "dot",
        /** 2c tints even the connected dot blue here: the whole bar changes
         * hue, dot included, so the row reads as one statement. */
        tone: "fd-tone-info",
        text: "connected",
        detail: latency,
      },
      action: {
        key: "Enter",
        label: `Reload for ${hostVersion}`,
        kind: "reload",
        tone: "info",
      },
      staleChip: null,
      note: null,
    };
  }
  return {
    frame: "neutral",
    message: state.seat === "observing" ? READ_ONLY : null,
    status: {
      glyph: "dot",
      tone: "fd-tone-ok",
      text: "connected",
      detail: latency,
    },
    action: null,
    staleChip: null,
    note: null,
  };
}

/**
 * Q5's row. The only one in 2c whose *mode chip* is replaced rather than
 * drained (`FLIGHTDECK STOPPED`), because "no mode" understates it: there is
 * no host.
 *
 * `selfInitiated` is the requirement with teeth — a browser that just quit
 * FlightDeck itself must be shown an acknowledgement of its own action, not a
 * failure. Same terminal state, different sentence, and the difference is the
 * whole reason the flag is on the wire.
 */
function stoppedStrip(state: AppState): ConnectionStrip {
  const shutdown = state.shutdown;
  return {
    frame: "stopped",
    message: shutdownMessage(shutdown),
    status: {
      /** Hollow, not filled: nobody is claiming anything is running. */
      glyph: "hollow",
      tone: "fd-tone-quiet",
      text: shutdownStatusText(shutdown),
      detail: shutdown === null || shutdown.atLabel === "" ? null : shutdown.atLabel,
    },
    action: null,
    staleChip: null,
    note: shutdownNote(shutdown),
  };
}

function shutdownMessage(shutdown: ShutdownState | null): string {
  if (shutdown === null) {
    /** `connection: "stopped"` with no frame recorded — the socket closed
     * without a `Shutdown`, so we say only what we know. */
    return "the connection closed for good";
  }
  const detail = shutdown.detail === "" ? "" : ` · ${shutdown.detail}`;
  switch (shutdown.reason) {
    case "host_quit":
      /** Q5: acknowledge the user's own action instead of reporting a failure. */
      return shutdown.selfInitiated
        ? `you quit FlightDeck from this tab${detail}`
        : `FlightDeck was quit on the machine${detail}`;
    case "server_stopped":
      return shutdown.selfInitiated
        ? `you stopped the web interface from this tab${detail}`
        : `the web interface was stopped on the machine — FlightDeck is still running${detail}`;
    case "token_revoked":
      return `this browser's access was withdrawn${detail}`;
    case "restarting":
      return `the host is restarting${detail}`;
    /** An unknown reason is still final (`should_retry` says no), and the
     * host's own `detail` is the part the user can act on, so it is never
     * dropped in favour of a tidier sentence of ours. */
    case "unknown":
      return shutdown.detail === ""
        ? "the host closed the connection and did not say why"
        : `the host closed the connection · ${shutdown.detail}`;
  }
}

function shutdownStatusText(shutdown: ShutdownState | null): string {
  if (shutdown === null) {
    return "connection closed";
  }
  switch (shutdown.reason) {
    case "host_quit":
      return "host exited cleanly";
    case "server_stopped":
      return "web interface stopped";
    case "token_revoked":
      return "access withdrawn";
    case "restarting":
      return "host restarting";
    case "unknown":
      return "host gone";
  }
}

function shutdownNote(shutdown: ShutdownState | null): string {
  if (shutdown === null) {
    return "reload once the host is back";
  }
  switch (shutdown.reason) {
    case "host_quit":
      return "start it again on the machine to reconnect";
    case "server_stopped":
      return "run Start Web Interface on the machine to reconnect";
    case "token_revoked":
      return "ask for a new code on the machine";
    case "restarting":
      return "this tab will reconnect on its own";
    case "unknown":
      return "reload once the host is back";
  }
}

function retryDetail(state: AppState): string | null {
  const retry = state.retry;
  if (retry === null) {
    return null;
  }
  return retry.inSeconds === null
    ? `attempt ${retry.attempt}`
    : `attempt ${retry.attempt} · retry in ${retry.inSeconds}s`;
}

function staleChipFor(state: AppState): string | null {
  return state.staleness === null
    ? null
    : `terminal stale ${state.staleness.ago}`;
}

/** `1204992` -> `1 204 992`, as 2d prints the replay cursor. */
function groupDigits(value: number): string {
  return String(value).replace(/\B(?=(\d{3})+(?!\d))/g, " ");
}

/**
 * The five treatments of artboard 2d, as one word.
 *
 * `live` full colour, caret blinking, your keys arrive.
 * `asleep` desaturated cool (`--fd-term-asleep`) — *your keystrokes go
 *   somewhere else*, and the picture is still current.
 * `stale` amber cast + scanlines + a frozen clock, caret gone — *this is a
 *   photograph and nothing you type is arriving*.
 * `asleep_stale` both, and legible as a third thing because **the scanlines are
 *   what survive both**: desaturation and the amber cast fight each other, the
 *   scanlines do not.
 * `catching_up` colour is back, so it is trustworthy, with the replay bar and
 *   the byte cursor visible; input queues until it lands.
 *
 * The precedence below is a claim: staleness outranks everything, because
 * "what you are looking at is not true any more" is the only one of these facts
 * that can make a user act on a lie. Catching-up outranks asleep because the
 * returning colour is itself the message.
 */
export type PaneTone = "live" | "asleep" | "stale" | "asleep_stale" | "catching_up";

export function paneTone(state: AppState): PaneTone {
  const asleep = state.mode === "app";
  if (isStale(state)) {
    return asleep ? "asleep_stale" : "stale";
  }
  if (state.connection === "catching_up") {
    return "catching_up";
  }
  return asleep ? "asleep" : "live";
}

/**
 * Whether the picture on screen is a photograph.
 *
 * Four ways to get there, and they are all the same fact from the user's side —
 * bytes have stopped arriving:
 *
 *   - the transport is down (`reconnecting`, `disconnected`);
 *   - the host said goodbye (`stopped`) or withdrew the credential (`revoked`);
 *   - an access screen is up, which 2b spells out in words: *"everything you
 *     can see below this dialog is a photograph from the moment access
 *     ended"*;
 *   - **we were just evicted** (2f): *"the terminal behind this dialog is stale
 *     from the moment you lost control"*. Note this stops being true the moment
 *     the user chooses `w Watch read-only` — an observer gets live bytes, and
 *     2d's rule is that colour means live. So it is the *prompt* that is stale,
 *     not the observing seat.
 *
 * `connecting` is deliberately absent: there is nothing on screen yet to be a
 * photograph of.
 *
 * **Exported because the clock lives elsewhere.** 2d's frozen clock and 2c's
 * `terminal stale 34s` are durations, and nothing under `state/` or `ui/` may
 * read a clock (`ui/tokens.guard.test.ts` rule 3). `wire/socket.ts` — which
 * owns `now` — asks *this* function whether the picture is frozen, rather than
 * keeping a second list of connection states that would have missed the two
 * stale states the transport cannot see: an access screen and an eviction.
 */
export function isStale(state: AppState): boolean {
  if (state.access !== null) {
    return true;
  }
  if (state.takeover?.kind === "evicted") {
    return true;
  }
  return (
    state.connection === "reconnecting" ||
    state.connection === "disconnected" ||
    state.connection === "revoked" ||
    state.connection === "stopped"
  );
}

/**
 * Whether this state costs the user control — i.e. whether the mode chip is a
 * lie (§5.1). Exported so the rule is asserted once, over every state, rather
 * than re-derived per component.
 *
 * **The seat, not the input lock.** A writer that is momentarily refused
 * because somebody else is mid-burst has not lost control: the lock frees
 * itself once they go quiet, and draining the mode chip on every hand-off would
 * make it flicker several times a minute. What the chip reports is whether this
 * tab may type *at all*, which is exactly what `seat` says. Who is typing right
 * now is the viewer chip's business (`state/seats.ts`).
 */
export function hasControl(state: AppState): boolean {
  return state.connection === "connected" && state.seat === "writing";
}

/** The coarse buckets, for tests that want to iterate every one of them. */
export const ALL_CONNECTION_STATUSES: readonly ConnectionStatus[] = [
  "connecting",
  "connected",
  "reconnecting",
  "catching_up",
  "disconnected",
  "revoked",
  "stopped",
];
