# Vendored design references

## Why these are here

The FlightDeck Web design lives in a Claude Design project
(`ba5af64b-7f47-4d58-a88e-6573ee172ac0`, "Web interface design briefing"). Reaching
it requires an authenticated `claude_design` / `DesignSync` MCP session, which is
not available in every environment and can lapse.

Implementation must not stall on that. These files are the committed copy of the
design at the point each turn was accepted, so anyone — human or agent — can read
the exact layout, spacing, copy and colour of what they are building without
network access or a live MCP login.

## Files

| File | Contents |
| --- | --- |
| `flightdeck-web-turn2.dc.html` | **Current.** Turn 2 (2a–2g) followed by turn 1 (1a–1h) in one document. Turn 2: access overlay in both states, the three browser-side access screens, every connection state, live/asleep/stale/asleep-and-stale/catching-up, the activity feed, the takeover trio, and the semantic reference sheet. |
| `flightdeck-web-turn1.dc.html` | Turn 1 as originally delivered, artboards 1a–1h: main screen in Terminal and App mode, split view ×3, filtered command palette, new-agent dialog (both states), configuration manager, destructive confirmation, and the stated positions. Kept as the record of that turn; the turn-2 file carries the same artboards unchanged. |

**Read `flightdeck-web-turn2.dc.html`** unless you specifically need the turn-1
record — it is a superset.

## How to read them

These are `.dc.html` documents: plain HTML with all styling inline, wrapped in an
`<x-dc>` element. **Read the markup — do not try to open it in a browser.** It
loads a `support.js` runtime that expects `window.React`, which is not vendored,
so it will not render standalone.

Reading the markup is the intended use and is entirely sufficient: every colour,
size, border and string is an inline literal. To find a region, search for its
banner comment — `1a — MAIN, TERMINAL MODE`, `1c — SPLIT VIEW`,
`1f — CONFIGURATION MANAGER`, and so on.

**The palette is now named by the designer**, in artboard `2g — REFERENCE SHEET`:
semantic custom properties (`--fd-text`, `--fd-stale`, `--fd-term-asleep`, …) with
their meanings and contrast. Ship those names, not literals. `2g` supersedes the
extracted table in `specs/WEBAPP_DESIGN_BRIEFING_T2.md` §7, which was the input
that produced it. Use these files for layout, structure and copy.

## Keeping them current

When a design turn is accepted, vendor it here rather than relying on the remote
project, and add a row to the table above. Never edit these files to change the
design — they are a record of what the designer delivered, and the project is
the place design changes happen.
