import type { AccessScreen, AccessState } from "./model";
import { ACCESS_CODE_LENGTH } from "./model";

/**
 * Artboard `2b — BROWSER-SIDE ACCESS SCREENS`: the words, per screen.
 *
 * Four screens, mirroring `credentials::AccessScreen` variant for variant —
 * `code_entry | rejected | revoked | rate_limited`. 2b draws three panels and
 * puts the rate-limit state in the third's footer, but the host models it as a
 * fourth because it is the one state where **retrying is refused rather than
 * merely useless**, and a button that cannot work must not be offered.
 *
 * Q7's posture governs every string below: *never claim a protection we cannot
 * deliver.* So nothing here says "secure", nothing says "encrypted", and the
 * revoked screen is explicit that the agents kept running and the picture
 * underneath is a photograph — because that is what is true.
 */

/**
 * Mirrors `credentials::RATE_LIMIT_LOCKOUT_MS` (60_000).
 *
 * 2b's exact sentence is "3 attempts left before this address is rate-limited
 * **for 60s**", so the browser has to know the lockout length before it has
 * ever been locked out — which means it cannot come from the refusal body,
 * which only carries `retry_after_ms` once the limiter has already fired.
 * Mirrored rather than derived, and listed in the task report as a value the
 * host could usefully publish (see also `RATE_LIMIT_MAX_FAILURES`, which the
 * browser deliberately does *not* mirror: the host sends
 * `attempts_remaining` on every refusal, so the budget itself is never guessed).
 */
export const RATE_LIMIT_LOCKOUT_SECONDS = 60;

/** Mirrors `credentials::BOOTSTRAP_CODE_TTL_MS` (120_000) — 2b: "good for two
 * minutes", "Codes last 120 seconds and only work once". */
export const BOOTSTRAP_CODE_TTL_SECONDS = 120;

export interface AccessCopy {
  readonly title: string;
  /** 2b's revoked panel puts `from the desktop` opposite the title. */
  readonly eyebrow: string | null;
  /** The paragraph in full-contrast text. */
  readonly body: string;
  /** The quieter second paragraph, or `null` when the screen has one line. */
  readonly detail: string | null;
  /** 2b's numbered recovery steps, in order. Empty on screens without them. */
  readonly steps: readonly string[];
  /** The primary button, or `null` when the screen refuses to offer one. */
  readonly primary: { readonly key: string; readonly label: string } | null;
  /** 2b's revoked panel offers `Esc Stay here` beside the primary action. */
  readonly secondary: { readonly key: string; readonly label: string } | null;
  /**
   * Which hue the panel takes. The same vocabulary the strip uses, and the
   * assignment 2b is emphatic about: **revoked is amber, not red — this is a
   * decision someone made, not a failure.**
   */
  readonly tone: "accent" | "alert" | "stale";
  /** Whether the four code boxes accept typing on this screen. */
  readonly acceptsCode: boolean;
}

export function accessCopy(access: AccessState): AccessCopy {
  switch (access.screen) {
    case "code_entry":
      return {
        title: "Enter your code",
        eyebrow: null,
        body:
          "On the machine running FlightDeck, press Ctrl-g and run Web Interface. " +
          "The overlay shows a four-digit code, good for two minutes.",
        /**
         * 2b prints this on the *first* screen, which reads oddly until you
         * notice what it is doing: a user who reached the code screen from a
         * working session needs to know why, and the two honest reasons are the
         * only two there are. It is never shown as an accusation.
         */
        detail:
          "Your last code stopped working because this browser's access was cleared, " +
          "or you are on a different browser.",
        steps: [],
        primary: { key: "Enter", label: "Connect" },
        secondary: null,
        tone: "accent",
        acceptsCode: true,
      };

    case "rejected":
      return {
        title: "That code did not work",
        eyebrow: null,
        body:
          `It expired, or it was mistyped. Codes last ${BOOTSTRAP_CODE_TTL_SECONDS} ` +
          "seconds and only work once.",
        detail: null,
        /** 2b's two numbered steps, verbatim — both of them are on the *other*
         * machine, which is the fact the user needs first. */
        steps: [
          "go to the machine running FlightDeck",
          "Ctrl-g → Web Interface → Space for a fresh code",
        ],
        primary: { key: "Enter", label: "Try again" },
        secondary: null,
        tone: "alert",
        acceptsCode: true,
      };

    case "revoked":
      return {
        title: "Access revoked",
        eyebrow: "from the desktop",
        body:
          "Someone at the FlightDeck machine withdrew this browser's access" +
          (access.revokedAgo === null ? "" : ` ${access.revokedAgo}`) +
          ". Nothing is broken and nothing is lost — you are simply no longer allowed in.",
        detail:
          "The agents kept running. Everything you can see below this dialog is a " +
          "photograph from the moment access ended.",
        steps: [],
        primary: { key: "Enter", label: "Enter a new code" },
        /** "Stay here" is a real choice: the photograph is still information,
         * and 2b refuses to clear the screen out from under someone reading it. */
        secondary: { key: "Esc", label: "Stay here" },
        tone: "stale",
        acceptsCode: false,
      };

    /**
     * Not drawn as a panel by 2b — it draws the rate limit as the footer line
     * of the rejected screen — so the copy below is written to 2b's rules
     * rather than copied from it, and is listed in the task report as the one
     * place a design turn would have something to say.
     *
     * Amber rather than red, by the same logic 2b applies to `revoked`: a
     * limiter doing its job is not a failure. And no primary button at all,
     * because the host will refuse it — offering `Try again` here would be the
     * app claiming an action it knows will not work.
     */
    case "rate_limited":
      return {
        title: "Too many attempts",
        eyebrow: "this address",
        body: lockoutSentence(access.lockoutSeconds),
        detail:
          "The limit is per address, not per code, so a fresh code from the desktop " +
          "will not shorten the wait.",
        steps: [],
        primary: null,
        secondary: null,
        tone: "stale",
        acceptsCode: false,
      };
  }
}

function lockoutSentence(lockoutSeconds: number | null): string {
  /** `null` means the host refused us without saying how long — which the
   * limiter does not normally do, so we say what we know and nothing more. */
  return lockoutSeconds === null
    ? "This address has spent its attempt budget. Try again shortly."
    : `This address has spent its attempt budget. Try again in ${lockoutSeconds}s.`;
}

/**
 * 2b's footer strip, verbatim: `3 attempts left before this address is
 * rate-limited for 60s`.
 *
 * The number is the **host's** (`CredentialStore::attempts_remaining`, sent on
 * every refusal). The browser never counts attempts itself: it would disagree
 * with the limiter that actually decides, and it is the limiter that is right.
 *
 * Returns `null` when there is nothing to say — no refusal yet — rather than
 * printing a full budget at a user who has not spent any of it.
 */
export function attemptsLine(access: AccessState): string | null {
  if (access.screen === "rate_limited") {
    const seconds = access.lockoutSeconds;
    return seconds === null
      ? "this address is rate-limited"
      : `this address is rate-limited for another ${seconds}s`;
  }
  const left = access.attemptsRemaining;
  if (left === null) {
    return null;
  }
  if (left === 0) {
    return "no attempts left — this address is about to be rate-limited";
  }
  const attempts = left === 1 ? "attempt" : "attempts";
  return `${left} ${attempts} left before this address is rate-limited for ${RATE_LIMIT_LOCKOUT_SECONDS}s`;
}

/**
 * The footer's left half: `no session · 192.168.2.14:7420`.
 *
 * The address comes from `location.host` and is printed only if we have it.
 * A fabricated address on a security screen would be the first lie the user
 * sees, and it is the one string on this screen they might check.
 */
export function accessFooter(host: string): string {
  return host === "" ? "no session" : `no session · ${host}`;
}

/** Whether `Enter` should attempt an exchange: a full code on a screen that
 * accepts one. Pure, so the keyboard rule is a unit test. */
export function canSubmit(access: AccessState): boolean {
  return (
    accessCopy(access).acceptsCode &&
    access.code.length === ACCESS_CODE_LENGTH
  );
}

/** The four boxes 2b draws, one per position: the digit, or `null` for empty.
 * The caret sits on the first empty box. */
export function codeBoxes(
  code: string,
): readonly { readonly digit: string | null; readonly caret: boolean }[] {
  const boxes = [];
  for (let i = 0; i < ACCESS_CODE_LENGTH; i += 1) {
    const digit = code[i] ?? null;
    boxes.push({ digit, caret: i === code.length });
  }
  return boxes;
}

/** The wire spellings `screen_name()` sends, for parsing a refusal body. */
export function parseAccessScreen(value: unknown): AccessScreen {
  switch (value) {
    case "rejected":
      return "rejected";
    case "revoked":
      return "revoked";
    case "rate_limited":
      return "rate_limited";
    /** Anything unrecognised — including a newer host's fifth screen — falls
     * back to the one screen that is always safe to show: ask for a code. */
    default:
      return "code_entry";
  }
}
