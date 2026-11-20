#!/usr/bin/env bash
# Tutorial: "Let the agent test its own work" (about 1:30).
#   Pinboard, an agent waiting for a task -> open the session browser and
#   switch "Agent" on -> prompt: build a note counter, then check it in the
#   browser -> watch the agent start the app, open it, add a note and read the
#   counter (sped up) -> its summary -> Stop / Agent off.
# Off camera: Pinboard open, the agent started (no prompt: it just waits);
# the browser tools are allowed in Claude Code's settings, so the agent's
# calls ask no permission.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
STATE="$ROOT/tutorials/worker/agent-state.sh"
W="${WORKER_NAME:-fd-tutorial-worker}"
AGENT="note count"
PROMPT="Show how many notes there are under the board's title, for example: 3 notes. Then check it yourself: start the dev server in the background, load localhost:3000 in the browser tab that's already open, add a note, and make sure the count goes up. Commit your work when you're done."

"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" pinboard "$AGENT" >/dev/null
# Allow the browser tools (MCP server flightdeck-browser) in Claude Code.
podman exec -u dev "$W" python3 -c '
import json
p = "/home/dev/.claude/settings.json"
s = json.load(open(p))
allow = s.setdefault("permissions", {}).setdefault("allow", [])
if "mcp__flightdeck-browser" not in allow:
    allow.append("mcp__flightdeck-browser")
json.dump(s, open(p, "w"), indent=2)
'
# Off camera: Pinboard open, the agent started.
"$VM" run - <<EOF
glide 855 766 0.3
click
pause 1.5
glide 1379 156 0.3
click
pause 1.8
glide 93 750 0.3
click
pause 1.2
type "$AGENT"
key enter
pause 6
glide 760 420 0.3
EOF
"$VM" record start

"$VM" run - <<EOF
caption "Agents can test their work in the browser"
pause 0.4
say "An agent can test its own work in the session's browser. It opens pages, clicks, types and reads the console, while you watch."
pause 1.0
waitvoice 0.3

caption "Open the session browser, and switch Agent on"
say "Open the session's browser. The agent may only use it once you switch it on, for this session."
glide 242 750 1.0
pause 0.3
click
pause 1.5
highlighttext "Agent: off" 0 8
waitvoice 0.3
clicktext "Agent: off" 0
pause 0.6
unhighlight
highlighttext "Agent: on" 0 8
pause 1.2
unhighlight

caption "Ask for a change, and a check in the browser"
say "Now give the agent its task, and ask it to check the result in the browser itself."
glide 93 181 1.0
pause 0.3
click
pause 0.8
type "$PROMPT" 45
pause 0.5
key enter
waitvoice 0.3

caption "Watch the agent work (sped up)"
waitfor 30 $STATE "$AGENT" working
say "Switch back to the browser, and watch."
glide 93 208 1.0
pause 0.3
click
pause 0.5
waitvoice 0.2
fast 3
waitfor 400 $STATE "$AGENT" idle
normal
pause 1.0

caption "It loaded the app, added a note, and checked the count"
say "The agent wrote the code, loaded the app in this tab, added a note, and saw the count go up. It's all there to see."
pause 1.0
waitvoice 0.4

caption "Its summary: checked in the browser, and committed"
say "Its summary says what it checked, and it committed its work."
glide 93 181 1.0
pause 0.3
click
pause 1.0
waitvoice 1.0

caption "Switch it off when you're done"
say "When you're done, switch it off again. Using the browser yourself, or pressing Stop while the agent works, switches it off too."
glide 93 208 1.0
pause 0.3
click
pause 0.8
highlight 1268 95 98 26
glide 1317 108 0.8
pause 0.3
click
pause 0.6
unhighlight
glide 760 420 0.8
waitvoice 1.0
caption
pause 0.6
cut
EOF

"$VM" record stop agent-browser
