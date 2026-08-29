import { paneTone } from "../state/connection";
import type { PaneTone } from "../state/connection";
import type { AppState } from "../state/types";
import { append, clear, el } from "./dom";
import type { Region } from "./dom";
import { createTerminalStage } from "./terminalStage";
import type { TerminalMount } from "./terminalStage";

/**
 * Region 5 of 7 — the terminal viewport, in all five treatments of artboard
 * `2d — ASLEEP vs STALE`.
 *
 * The distinction 2d makes carefully, and this app must not blur:
 *
 *   **asleep** — "your keystrokes are going somewhere else". The picture is
 *   current and true; only the keyboard has moved. Desaturates cool
 *   (`--fd-term-asleep`, lifted to 5.6:1 precisely because a whole screen of
 *   text was the palette's least defensible dim value) and drops bold.
 *
 *   **stale** — "this picture is a photograph and nothing you type is
 *   arriving". Amber cast, scanlines, a frozen clock, **caret gone**. The caret
 *   is the one that matters most: a blinking caret is the strongest "I am
 *   listening" signal a terminal has, so leaving it blinking on a photograph
 *   would undo everything the amber cast is trying to say.
 *
 *   **asleep + stale** — both at once, and legible as a *third* state because
 *   **the scanlines are what survive both**. Desaturation and an amber cast
 *   fight each other; the scanline overlay does not, so it is the carrier.
 *
 *   **catching up** — colour is back, so the picture is trustworthy again, with
 *   the replay bar and Q3's byte cursor on show. Input queues until it lands.
 *
 * Which of the five applies is `paneTone`, a pure function, so the precedence
 * is unit-tested rather than emergent. This component only draws it.
 */
export function createTerminalPane(mount: TerminalMount): Region {
  const stage = createTerminalStage({
    terminalId: (state) => state.selection?.terminalId ?? null,
    mount,
    label: "terminal",
  });

  const foot = el("div", { class: "fd-pane__foot" }, [
    "terminal asleep — keystrokes go to FlightDeck · ",
    el("span", { class: "fd-key", text: "Enter" }),
    " or click to wake it",
  ]);

  /**
   * The scanline overlay. A single element with no text, `aria-hidden`, and
   * **`pointer-events: none`** in CSS — it lies over the terminal, so a version
   * that swallowed clicks would break click-to-focus in exactly the state where
   * the user is most likely to click.
   */
  const scanlines = el("div", {
    class: "fd-pane__scanlines",
    attrs: { "aria-hidden": "true" },
  });
  /** The banner 2d puts at the bottom of a stale or replaying terminal. */
  const banner = el("div", { class: "fd-pane__banner", attrs: { role: "note" } });

  const pane = el(
    "div",
    { class: "fd-pane", attrs: { "data-tone": "live", "data-caret": "on" } },
    [stage.el, scanlines, banner, foot],
  );

  return {
    el: pane,
    update(state: AppState) {
      const tone = paneTone(state);
      pane.setAttribute("data-tone", tone);
      /**
       * 2d: the caret is removed on a photograph, not merely stopped. The
       * attribute is what CSS hides `.xterm-cursor` off, and it is also how a
       * test asserts the rule without a real xterm instance in the DOM.
       */
      pane.setAttribute("data-caret", frozen(tone) ? "off" : "on");
      scanlines.hidden = !frozen(tone);
      renderBanner(banner, state, tone);
      stage.update(state);
    },
  };
}

/** The two tones that are photographs — the scanline carriers. */
function frozen(tone: PaneTone): boolean {
  return tone === "stale" || tone === "asleep_stale";
}

function renderBanner(banner: HTMLElement, state: AppState, tone: PaneTone): void {
  clear(banner);
  if (tone === "catching_up") {
    banner.hidden = false;
    banner.append(...replayChildren(state));
    return;
  }
  if (!frozen(tone)) {
    /**
     * Q3's warning outlives the state that produced it.
     *
     * The host answers a resume with **one** frame per terminal, so a
     * truncated replay can be over in milliseconds — and a sentence nobody can
     * read is the same silence the flag exists to break. `wire/socket.ts`
     * therefore leaves `replay` in place for a few seconds after the drain,
     * with `bytesDone === bytesTotal`. Nothing here claims a replay is still
     * running: the bar and the outstanding count belong to the branch above,
     * and this prints only the loss, in the past tense.
     */
    if (state.replay?.truncated === true) {
      banner.hidden = false;
      append(banner, [
        el("span", {
          class: "fd-pane__banner-text",
          text: "output older than the host's buffer was lost — the terminal above has a gap in it",
        }),
      ]);
      return;
    }
    banner.hidden = true;
    return;
  }
  banner.hidden = false;

  const ago = state.staleness?.ago ?? null;
  append(banner, [
    /**
     * The frozen clock. 2d prints it as a wall-clock time, not a duration,
     * because "16:41:08" is checkable against the user's own memory of when
     * they last looked — a duration is not.
     */
    state.staleness === null
      ? null
      : el("span", {
          class: "fd-pane__clock",
          text: state.staleness.frozenAt,
          title: "the time of the last byte that arrived",
        }),
    el("span", {
      class: "fd-pane__banner-text",
      /**
       * `ago` is the transport's, ticking (`staleness/set`). When it is
       * genuinely absent the clause is **dropped**, not filled in: this used to
       * read `frozen a moment ago`, which is a duration nobody measured, and it
       * hid the fact that nothing was computing one at all for the whole of
       * turn 2. An unmeasured age says nothing rather than something small.
       */
      text:
        tone === "asleep_stale"
          ? /** 2d's own wording: the second clause matters, because in App mode
             * the keys would not have reached the terminal anyway, and a user
             * who does not know that will blame the connection for both. */
            ago === null
            ? "this is a photograph · and FlightDeck has focus, so keys would go to the sidebar anyway."
            : `frozen ${ago} ago · and FlightDeck has focus, so keys would go to the sidebar anyway.`
          : ago === null
            ? "this is a photograph. Nothing you type is arriving."
            : `frozen ${ago} ago — this is a photograph. Nothing you type is arriving.`,
    }),
    /**
     * §5.1 made visible where the user is actually looking. The status bar
     * promises `keystrokes are being held`; this says how many, on the surface
     * they are typing at. A promise with a number on it is one a user can check.
     */
    state.pendingInput.length === 0
      ? null
      : el("span", {
          class: "fd-pane__held",
          text: `${state.pendingInput.length} keystroke${
            state.pendingInput.length === 1 ? "" : "s"
          } held`,
          title:
            "queued in order and sent when the link returns — nothing typed here is discarded (§5.1)",
        }),
  ]);
}

/** 2d's catching-up pane: the bar, the byte count, and Q3's cursor. */
function replayChildren(state: AppState): readonly Node[] {
  const replay = state.replay;
  if (replay === null) {
    return [
      el("span", {
        class: "fd-pane__banner-text",
        text: "live again · replaying what you missed",
      }),
    ];
  }
  const children: Node[] = [];
  /**
   * A real `<progress>`, not a styled `<div>` with a width. Two reasons: a
   * width would have to be an inline style, which the palette guard forbids in
   * `ui/` for good reason, and `<progress>` is the element that already means
   * this to a screen reader.
   */
  const bar = el("progress", {
    class: "fd-replay",
    attrs: {
      value: String(replay.bytesDone),
      max: String(Math.max(replay.bytesTotal, replay.bytesDone, 1)),
      "aria-label": "replay progress",
    },
  });
  children.push(bar);
  children.push(
    el("span", {
      class: "fd-pane__banner-text",
      text: `replaying ${formatBytes(replay.bytesTotal - replay.bytesDone)}…`,
    }),
  );
  children.push(
    el("span", {
      class: "fd-pane__held",
      text: replay.truncated
        ? /** Q3: the ring aged out, so continuity is broken. Saying so is the
           * whole reason `truncated` is on the wire — a viewer that pretended
           * continuity would show a terminal with a hole in it. */
          "output older than the host's buffer was lost — this is not a continuous replay"
        : `from byte ${groupDigits(replay.fromByte)}`,
    }),
  );
  return children;
}

/** `41984` -> `41 KB`, as 2d prints the outstanding replay. */
function formatBytes(bytes: number): string {
  const safe = Math.max(0, bytes);
  if (safe < 1024) {
    return `${safe} B`;
  }
  if (safe < 1024 * 1024) {
    return `${Math.round(safe / 1024)} KB`;
  }
  return `${(safe / (1024 * 1024)).toFixed(1)} MB`;
}

/** `1204992` -> `1 204 992`, as 2d prints the replay cursor. */
function groupDigits(value: number): string {
  return String(value).replace(/\B(?=(\d{3})+(?!\d))/g, " ");
}
