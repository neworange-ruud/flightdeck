import type {
  Project,
  Session,
  SessionStatus,
  StatusGlyph,
  StatusTone,
} from "./model";

/**
 * The status vocabulary, in one pure place.
 *
 * Every colour decision here is a *token name* (2g), never a hex value, and
 * every label is the exact string artboard 1a renders. Two components need
 * these answers (the project tab row and the sidebar) and so does the test
 * suite, which is why this is a module and not inline markup.
 */

/** The chip 1a puts after the agent name: `[in progress]`, `[idle]`, … */
export function statusLabel(status: SessionStatus): string {
  switch (status) {
    case "in_progress":
      return "[in progress]";
    case "idle":
      return "[idle]";
    case "waiting":
      return "[waiting]";
    case "error":
      return "[error]";
    case "reviewing":
      return "[reviewing]";
    case "starting":
      return "[starting]";
    /**
     * §5.1: no lifecycle hooks means there is no transition to report, so the
     * app says so twice rather than guessing once. This deliberately does not
     * look like the other labels — it is not a status, it is the absence of
     * one.
     */
    case "unknown":
      return "unknown → unknown";
  }
}

export function statusTone(status: SessionStatus): StatusTone {
  switch (status) {
    case "in_progress":
    case "reviewing":
      return "accent";
    case "idle":
      return "ok";
    case "waiting":
    case "error":
      return "alert";
    /** `starting` and `unknown` are facts about what we do not know, so they
     * sit on the lifted floor (--fd-text-quiet, 4.8:1), not on --fd-text-decor. */
    case "starting":
    case "unknown":
      return "quiet";
  }
}

export function statusGlyph(status: SessionStatus): StatusGlyph {
  switch (status) {
    /** work is happening, so the glyph moves (1a's `{{ spinner }}`) */
    case "in_progress":
    case "starting":
      return "spinner";
    /** hollow: nobody is claiming to know (§5.1's `○`) */
    case "unknown":
      return "hollow";
    case "idle":
    case "waiting":
    case "error":
    case "reviewing":
      return "dot";
  }
}

/**
 * The project tab's dot, following the precedence §5.1 states for the unread
 * chip and 1a renders on the tabs: **attention beats finished beats quiet**,
 * with work-in-progress shown as motion because it is the one state that will
 * change on its own.
 *
 * 1a is the fixed point: `flightdeck` (in progress + waiting + error) shows a
 * spinner, `api-gateway` (waiting) an alert dot, `web` (all idle) an ok dot.
 * So motion outranks attention, and attention outranks calm.
 */
export function projectGlyph(project: Project): StatusGlyph {
  if (project.sessions.some((s) => statusGlyph(s.status) === "spinner")) {
    return "spinner";
  }
  if (project.sessions.some((s) => statusTone(s.status) === "alert")) {
    return "dot";
  }
  if (project.sessions.every((s) => s.status === "unknown")) {
    return "hollow";
  }
  return "dot";
}

export function projectTone(project: Project): StatusTone {
  if (project.sessions.some((s) => statusGlyph(s.status) === "spinner")) {
    return "alert";
  }
  if (project.sessions.some((s) => statusTone(s.status) === "alert")) {
    return "alert";
  }
  if (project.sessions.length === 0) {
    return "quiet";
  }
  if (project.sessions.every((s) => s.status === "unknown")) {
    return "quiet";
  }
  return "ok";
}

/**
 * The sidebar's second line, after the agent name. Returns the label plus the
 * lifecycle note when the agent has no hooks, so §5.1's full string
 * (`unknown → unknown · Codex CLI reports no lifecycle`) exists in exactly one
 * place and can be asserted verbatim.
 */
export function sessionStatusText(session: Session): string {
  if (session.startingNote !== null) {
    return session.startingNote;
  }
  const label = statusLabel(session.status);
  return session.lifecycleNote === null
    ? label
    : `${label} · ${session.lifecycleNote}`;
}

/**
 * The bare status word, without 1a's brackets — used by the `really: idle`
 * line, where the brackets would read as a second chip rather than as the
 * truth underneath a manual override.
 */
export function statusWord(status: SessionStatus): string {
  return status === "in_progress" ? "in progress" : status;
}
