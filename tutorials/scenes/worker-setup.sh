#!/usr/bin/env bash
# Tutorial: "Set up a worker on your server" (about 1:40).
#   Terminal: ssh devbox (a bare Linux server) -> the installer ->
#   flightdeck worker enable -> choose the agents it offers -> sudo reboot ->
#   ssh in again: the worker is back by itself -> worker pair (the next video
#   connects the Desktop).
#
# The stage is MODE=server (tutorials/worker/Containerfile.server): systemd as
# init, FlightDeck not installed. The installer is the real one and installs
# the real release; off camera, the scene then puts the tutorial's debug build
# in its place, because a release worker listens on Tailscale only.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
W="${WORKER_NAME:-fd-tutorial-worker}"
INSTALLED=/home/dev/.cargo/bin/flightdeck

MODE=server "$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/vm/terminal.sh"
"$VM" record start

"$VM" run - <<EOF
caption "A worker: a machine that runs your agents"
pause 0.5
say "A worker is a machine that stays on and runs your agents: a Linux server, or a Mac mini under the desk. This is devbox, a fresh Linux server in my tailnet. Let's set it up."
pause 1.0
type "ssh devbox"
pause 0.3
key enter
waitvoice 0.3

caption "Install FlightDeck: one command, no root"
say "First, install FlightDeck. One command downloads it into your home folder. It needs no root."
pause 0.4
type "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/neworange-ruud/flightdeck/releases/latest/download/flightdeck-installer.sh | sh" 45
pause 0.4
key enter
waitfor 120 podman exec -u dev $W test -f /home/dev/.cargo/env
pause 2.0
waitvoice 0.3
cut
sh podman exec -u dev $W cp /usr/local/lib/flightdeck/flightdeck $INSTALLED
pause 0.5
uncut
say "As it says, load the new path into this shell."
type "source \$HOME/.cargo/env"
pause 0.3
key enter
waitvoice 0.3
key ctrl-l

caption "Start the worker, now and at every boot"
say "Now one command starts the worker, and hands it to systemd, so it comes back by itself after a reboot. If your account needs lingering for that, it tells you how to turn it on."
pause 0.4
type "flightdeck worker enable"
pause 0.3
key enter
pause 3.0
highlight 4 92 875 19
waitvoice 0.4
unhighlight
key ctrl-l

caption "Choose the agents it offers"
say "The worker offers every agent FlightDeck supports. This box has only Claude Code set up, so switch the others off."
pause 0.3
type "flightdeck worker agents"
pause 0.3
key enter
pause 1.0
highlight 4 128 400 19
pause 1.5
unhighlight
type "for a in codex cursor opencode; do flightdeck worker agents disable \$a; done"
pause 0.3
key enter
waitvoice 0.3
pause 2.5
key ctrl-l

caption "Reboot: the worker comes back by itself"
say "Let's check that. Reboot the server."
pause 0.3
type "sudo reboot"
pause 0.3
key enter
waitvoice 0.3
fast 4
pause 4
waitfor 90 bash -c "podman exec $W systemctl is-active -q dev-sshd && podman exec -u dev $W $INSTALLED worker status --json | grep -q '\"running\"'"
pause 1
normal
say "Once it's back up, log in again, and ask the worker how it's doing."
type "ssh devbox"
pause 0.3
key enter
pause 1.0
type "flightdeck worker status"
pause 0.3
key enter
pause 2.5
waitvoice 0.2
say "It's running, started at boot. Nobody had to log in."
# Fixed rows: sudo reboot, the closed connection, ssh, status, then its lines.
highlight 3 146 400 19
pause 1.4
highlight 3 290 470 19
waitvoice 0.4
unhighlight

caption "Next: pair FlightDeck Desktop"
say "The worker is ready. Last step: pair FlightDeck Desktop with it, with flightdeck worker pair. The next video shows how."
pause 0.3
type "flightdeck worker pair"
pause 0.3
key enter
waitvoice 1.2
caption
pause 0.6
cut
EOF

"$VM" record stop worker-setup
