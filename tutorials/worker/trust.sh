#!/usr/bin/env bash
# Mark the worktrees a tutorial will create as trusted by Claude Code, so the
# agent starts without its "Do you trust this folder?" prompt. Claude Code
# asks per folder (a worktree is its own git root, so trusting the project
# above it is not enough); a scripted tutorial knows its task names up front.
#
#   tutorials/worker/trust.sh pinboard "delete notes" "dark theme"
#
# A task name becomes the worktree folder the way FlightDeck names branches:
# lower case, spaces as dashes ("delete notes" -> delete-notes).
#
# It also marks Claude Code's launch announcements (e.g. "Fable 5.2 is here")
# and notices as seen, and puts off its API-credit upsell, so they stay out
# of the video.
set -euo pipefail
NAME="${WORKER_NAME:-fd-tutorial-worker}"
project="$1"; shift
for task in "$@"; do
  slug="$(echo "$task" | tr '[:upper:]' '[:lower:]' | tr ' ' '-')"
  echo "/home/dev/flightdeck/$project/.flightdeck/worktrees/$slug"
done | podman exec -i "$NAME" python3 -c '
import json, os, sys
p = os.path.expanduser("~/.claude/.claude.json")
cfg = json.load(open(p))
for path in sys.stdin.read().split():
    cfg.setdefault("projects", {}).setdefault(path, {})["hasTrustDialogAccepted"] = True
    print("trusted", path)
seen = cfg.setdefault("announcementImpressions", {})
for key in seen:
    seen[key] = 999
for key in cfg.get("seenNotifications", {}):
    cfg["seenNotifications"][key] = 999
for org in cfg.get("apiCreditLinkStatusCache", {}).values():
    org["nextAskAt"] = 4102444800000  # 2100
json.dump(cfg, open(p, "w"), indent=2)
'
