#!/usr/bin/env bash
# Tutorial: "FlightDeck in your terminal" (about 1:45).
#   Terminal: ssh devbox -> `flightdeck` in pinboard -> Ctrl-N, pick Claude
#   Code, name the task -> prompt -> the status bar back to app mode ->
#   a second agent -> the sidebar while both work (sped up) -> + shell in a
#   worktree: git log -> Ctrl-G palette: Finish -> merged.
#
# The stage is MODE=noworker: the terminal UI refuses to start on a machine
# where a worker serves the projects, so devbox runs no worker here.
#
# Keys: the Mac's Terminal sends Shift-Esc as a plain Esc (which would
# interrupt the agent), so the scene leaves terminal mode by clicking the
# status bar's mode hint, which the TUI makes a button. The New Agent dialog
# starts on the configured default backend (OpenCode unless set), so Claude
# Code is three rows up.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
STATE="$ROOT/tutorials/worker/tui-agent-state.sh"
FIRST="dark theme"
SECOND="search"
P1="Add a dark theme that follows the system setting. Only touch public/style.css. Commit your work when you're done."
P2="Add a search box above the board that filters the notes as you type. Leave public/style.css alone. Commit your work when you're done."

MODE=noworker "$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" pinboard "$FIRST" "$SECOND" >/dev/null
"$ROOT/tutorials/vm/terminal.sh"
"$VM" record start

"$VM" run - <<EOF
caption "FlightDeck in your terminal"
pause 0.4
say "FlightDeck also runs right in your terminal, on any machine you can reach. Here, it's devbox, over SSH."
pause 0.8
type "ssh devbox"
pause 0.3
key enter
pause 1.0
waitvoice 0.2
say "Go to a project, and start FlightDeck."
type "cd ~/flightdeck/pinboard && flightdeck"
pause 0.3
key enter
pause 2.0
waitvoice 0.3

caption "Control-N: a new agent on its own branch"
say "Press Control N for a new agent. Pick the backend, and name the task. Like in FlightDeck Desktop, it gets its own branch and worktree."
pause 0.6
key ctrl-n
pause 1.2
key up
pause 0.3
key up
pause 0.3
key up
pause 0.6
type "$FIRST" 18
pause 0.5
key enter
pause 2.5
key enter
pause 0.6
waitvoice 0.2

caption "Enter: type to the agent"
say "Enter puts you in the agent's terminal. Tell it what to do."
key enter
pause 0.6
type "$P1" 45
pause 0.4
key enter
waitvoice 0.3

caption "Back to FlightDeck's keys: click the mode, or Shift-Esc"
say "To start another, switch back to FlightDeck's own keys: click the mode in the status bar, or press shift escape."
pause 0.4
highlight 268 775 340 20
glide 506 785 1.0
pause 0.3
click
pause 0.8
waitvoice 0.2
unhighlight

caption "A second agent, side by side"
say "Then Control N again, for a second task."
key ctrl-n
pause 1.0
key up
pause 0.3
key up
pause 0.3
key up
pause 0.5
type "$SECOND" 18
pause 0.4
key enter
pause 2.5
key enter
pause 0.5
key enter
pause 0.5
type "$P2" 45
pause 0.4
key enter
waitvoice 0.3

caption "The sidebar shows what every agent is doing (sped up)"
say "The sidebar shows every agent, and what it's doing."
highlight 8 160 252 110
fast 6
waitfor 60 $STATE "$SECOND" working
waitfor 400 $STATE "$FIRST" idle
waitfor 400 $STATE "$SECOND" idle
normal
pause 0.6
waitvoice 0.2
unhighlight

caption "A shell in an agent's worktree"
say "Click an agent to look in. Plus shell opens a shell in its worktree, on its branch."
clicktext "$FIRST" 0
pause 1.0
glide 1371 137 0.8
pause 0.3
click
pause 2.0
# Clicking an agent leaves app mode on: Enter puts the keys in the new shell.
key enter
pause 0.5
type "git log --oneline -3"
pause 0.3
key enter
pause 1.0
waitvoice 0.6

caption "Control-G: every action, in the palette"
say "Control G opens the palette, with every action. Finish merges the agent's branch into main."
key ctrl-g
pause 0.8
type "finish" 14
pause 0.8
key enter
pause 1.2
waitvoice 0.2
type "y"
pause 2.0
say "Merged, and the worktree is cleaned up."
waitvoice 0.5
key enter
pause 0.6

caption "Same FlightDeck: flightdeck connect drives a worker too"
say "It's the same FlightDeck as the Desktop app. And flightdeck connect drives a worker from your terminal the same way."
waitvoice 1.0
caption
pause 0.6
cut
EOF

"$VM" record stop terminal-ui
