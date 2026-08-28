import type { HelpRow } from "./model";

/**
 * The half of the help overlay the **browser** owns
 * (`remote-control-ll5.8`, `specs/WEB_INTERFACE.md` §6.5 R16).
 *
 * ## Why the overlay has two halves at all
 *
 * The host sends its own keyboard (`Snapshot::help`, from `src/tui/help.rs`),
 * and rendering that list unqualified in a browser tab would be the app's first
 * outright lie: `Ctrl-q` in this tab does not quit FlightDeck, it sends `0x11`
 * to whatever the agent is running. Every chord in the host's list behaves that
 * way, because the SPA claims exactly one chord (§5's "`Ctrl-g` is the only
 * chord the app claims") and passes the rest to the PTY.
 *
 * So the panel states both, each labelled with where it acts. That is D16's
 * position — *"such actions remain visible and honest about where their effect
 * lands rather than being hidden"* — applied to keys instead of to commands,
 * and it is why the host's group carries the same `host only` badge D16 gives
 * `Open Worktree in File Manager`. One badge on the group heading rather than
 * thirty on the rows: the badge exists to be noticed, and thirty of them are
 * wallpaper.
 *
 * ## Why *this* list is authored here and not sent
 *
 * These are facts about a browser tab. The host does not run it, has no way to
 * know what it binds, and a host that claimed to would be guessing — the exact
 * failure the host-sent inventory (R7) and the host-sent help exist to prevent,
 * pointed the other way. Every row below is implemented in `ui/app.ts`'s
 * keydown handler or its click handler; if you change one there, change it
 * here, because there is no third place either could be read from.
 *
 * ## Why `Esc` gets a row of its own
 *
 * §5 makes a single `Esc` pass through to the agent, deliberately, because
 * `esc to interrupt` is the key users press most. A help screen that listed
 * `Esc Esc` and said nothing about `Esc` would leave the reader assuming the
 * app eats it — so the pass-through is stated as a row, not as a footnote.
 */
export const BROWSER_KEYS: readonly HelpRow[] = [
  { keys: "Ctrl-g", description: "Command palette — the only chord this tab claims" },
  { keys: "Esc Esc", description: "Leave terminal focus (within 400 ms)" },
  { keys: "Esc", description: "Passes through to the agent, always" },
  { keys: "Enter", description: "Focus the terminal, in App mode" },
  { keys: "a", description: "Activity feed, in App mode" },
  { keys: "?", description: "This help, in App mode" },
  { keys: "Click outside", description: "Release the keys back to the app" },
];

/** The heading the browser's own keys sit under. */
export const BROWSER_SECTION_TITLE = "This browser";

/** The heading the host's sections sit under. */
export const HOST_SECTION_TITLE = "On the host";

/**
 * The one sentence between the two halves.
 *
 * It says the two things a reader needs before reading thirty chords that do
 * not work here: where they act, and how to reach them anyway. The second
 * clause is not consolation — the palette really is every one of them (§5:
 * "palette-primary … everything else is a searchable command"), which is what
 * makes stating the host's keys useful rather than merely honest.
 */
export const HOST_SECTION_NOTE =
  "These act at the machine running FlightDeck, not in this tab — a chord typed here goes to the agent. Every one of them is also a row in the command palette, which does reach the host.";

/** What the panel says when the host sent no help at all. */
export const HOST_HELP_ABSENT =
  "This FlightDeck did not send its keybindings, so they are not shown. Nothing is missing from the list above — it is this tab's own keyboard, which this tab knows.";
