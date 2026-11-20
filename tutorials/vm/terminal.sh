#!/usr/bin/env bash
# Open the Mac's Terminal for a scene, the same way every take: a fresh
# window filling the screen (Window > Fill), the font two steps up from the
# default, the screen cleared. Text rows are then 18 points apart, the
# first at y 83, starting at x 9 (screen points).
#
# Terminal is not scripted with AppleScript: from an SSH session that needs
# an Automation permission the guest does not have.
set -euo pipefail
VM="$(dirname "${BASH_SOURCE[0]}")/vm"

# A blinking cursor (DECSCUSR 1, which ssh sessions keep): screencapture records
# a frame only when the screen changes, and after a long still stretch it
# stamps the first new frames too early, so a typed character would show up
# seconds before it was typed. The blink keeps frames coming.
"$VM" ssh 'grep -q "e\[1 q" ~/.zshrc 2>/dev/null || printf "%s\n" "printf '"'"'\\e[1 q'"'"'" >> ~/.zshrc
killall Terminal 2>/dev/null; sleep 2
rm -rf ~/Library/Saved\ Application\ State/com.apple.Terminal.savedState
defaults write com.apple.Terminal NSQuitAlwaysKeepsWindows -bool false
open -a Terminal
# Now and then Terminal starts with no window (the scene would then type into
# whatever is behind it): wait for a window shell (login, a child of
# Terminal), and open a window on the home folder if none comes.
for i in $(seq 1 20); do pgrep -P "$(pgrep -x Terminal)" login >/dev/null 2>&1 && break; sleep 0.25; done
pgrep -P "$(pgrep -x Terminal)" login >/dev/null 2>&1 || open -a Terminal ~'
sleep 3
"$VM" run - <<'EOF'
move 720 330
click
pause 0.5
key cmd-0 cmd-= cmd-=
pause 0.5
move 298 14
click
pause 0.6
move 300 96
click
pause 1
key cmd-k
move 1100 650
EOF
