# Known UI coordinates

All values are screen points on the 1440x810 VM display, with FlightDeck
Desktop full screen and paired with `devbox` (the state `tutorials/prepare.sh`
leaves).

They were measured against FlightDeck Desktop 2.0.x. A UI change can move any
of them, so check a new target, or any coordinate after a release, against a
`tutorials/vm/vm shot` preview (preview px × 1.125 = points). Any target that
an agent or the timing places has no fixed coordinate. Use `clicktext` or
`highlighttext` for those.

## Window and tabs

| Target | Point |
| --- | --- |
| Projects tab | 240,21 |
| Mission control tab | 375,21 |
| Worker tab | 508,21 |
| First project tab (e.g. pinboard) | 592,21 |
| Transfer files… | 1180,21 |
| Command palette button | 1340,21 |
| A safe place to park the pointer | 760,420 (or 1100,420 during setup) |

## Opening projects

| Target | Point |
| --- | --- |
| Open a project… (empty window) | 855,766 |
| The pinboard row's Open (project list) | 1379,156 |
| With no project open, rows sit lower: the `flightdeck/` row | 63,273 |
| New project name field (no project open) | 600,422 |
| Create (no project open) | 1385,422 |

## A project window

| Target | Point |
| --- | --- |
| New agent | 93,750 |
| New terminal | 199,750 |
| Session browser (globe) | 242,750 |
| Sidebar: agent row (first) | 93,181 |
| Sidebar: shell row (after the terminal button) | 93,208 |
| Session browser address bar | 788,108 |
| Finish (agent header) | 1302,64 |
| Merge (Finish dialog) | 922,447 |
| Side by side (Worker tab) | 1389,61 |
| Add files… (Transfer files dialog) | 506,408 |
| Reconnect (after another window took the worker) | 719,417 |

Agents are switched with `key opt-1`, `opt-2`, …; click into the terminal
area (around 840,400) before typing to an agent.

## Connecting (prepare.sh)

| Target | Point |
| --- | --- |
| Connect to remote… | 849,516 |
| Address field | 720,305 |
| Pairing code field | 720,370 |
| Connect | 505,457 |
| Connect anyway (not-encrypted warning) | 536,565 |

## The Mac's Terminal (tutorials/vm/terminal.sh)

The window fills the screen, with the font two steps up. Rows are 18 pt apart
and the first row sits at y 83. To fill the screen by hand: Window menu at
298,14, then Fill at 300,96.
