#!/usr/bin/env bash
# Tutorial: "Files and images" (about 1:50).
#   Transfer files…: upload a logo from the Mac into the agent's worktree ->
#   copy a mockup image -> Cmd-V in the agent's terminal -> the agent builds
#   it and puts the logo in the header -> the session browser shows it.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
STATE="$ROOT/tutorials/worker/agent-state.sh"
AGENT="note style"

"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" pinboard "$AGENT" >/dev/null
# The two images on the Mac's Desktop, the mockup in Preview to copy from.
for f in logo mockup; do "$VM" ssh "cat > ~/Desktop/$f.png" < "$ROOT/tutorials/assets/$f.png"; done
"$VM" run - <<EOF
glide 855 766 0.3
click
pause 1.5
glide 1379 156 0.3
click
pause 2
glide 93 750 0.3
click
pause 1
type "$AGENT"
key enter
pause 6
move 760 420
EOF
"$VM" record start

"$VM" run - <<EOF
caption "Files and images on the worker"
pause 0.4
say "The agent runs on the worker, but your files are on your Mac: a logo, a design, a data file. Here's how to get them to the agent."
waitvoice 0.3

caption "Transfer files…: upload from your Mac"
say "Transfer files uploads files from your Mac to the worker. It starts in the selected agent's worktree."
glide 1180 21 1.0
pause 0.3
click
pause 1.2
highlight 400 455 488 34
waitvoice 0.2
unhighlight
glide 506 408 0.8
pause 0.3
click
pause 1.4
glide 359 275 0.8
pause 0.3
click
pause 1.0
glide 510 194 0.8
pause 0.3
click
pause 0.6
glide 1101 529 0.8
pause 0.3
click
pause 1.2
glide 940 519 0.9
pause 0.3
click
pause 1.4
highlight 452 386 524 24
say "The logo is now in the agent's worktree, on devbox."
waitvoice 0.4
unhighlight
glide 945 451 0.6
pause 0.3
click
pause 0.6

caption "Paste an image straight into the agent"
say "For a picture, like a screenshot or a mockup, you don't even need to upload it. Copy it on your Mac..."
sh $ROOT/tutorials/vm/vm ssh "open -a Preview ~/Desktop/mockup.png"
pause 2.5
key cmd-a
pause 0.4
key cmd-c
pause 0.8
waitvoice 0.2
key cmd-q
pause 0.8
key ctrl-right
pause 1.5
glide 93 181 0.8
pause 0.3
click
pause 0.8
move 840 400
click
pause 0.5
say "...and paste it into the agent's terminal. FlightDeck copies the image to the worker and types its path, so the agent can see it."
pause 1.0
key cmd-v
pause 1.5
highlight 290 203 470 22
waitvoice 0.3
unhighlight

caption "Ask the agent to build it"
say "Now tell the agent what to do with both."
type " Style the notes like this mockup. Also put logo.png in the header, next to the title. Commit your work when you're done." 30
pause 0.4
key enter
waitfor 60 $STATE "$AGENT" working
waitvoice 0.2

caption "The agent works on devbox (sped up)"
fast 6
waitfor 400 $STATE "$AGENT" idle
normal
pause 1

caption "The result, in the session's browser"
say "Let's see it. Start the app, and open it in the session's browser."
glide 199 750 1.0
pause 0.3
click
pause 1.0
glide 93 208 0.8
pause 0.3
click
pause 0.6
type "npm run dev"
pause 0.3
key enter
pause 1.5
glide 242 750 1.0
pause 0.3
click
pause 1.4
glide 788 108 0.9
pause 0.3
click
pause 0.4
type "localhost:3000"
pause 0.3
key enter
pause 2.5
glide 760 420 0.8
waitvoice 0.5
highlight 316 158 118 28
highlight 318 255 1080 212
say "The notes match the mockup, and the logo you uploaded is in the header."
waitvoice 1.2
unhighlight
caption
pause 0.6
cut
EOF

"$VM" record stop files-and-images
