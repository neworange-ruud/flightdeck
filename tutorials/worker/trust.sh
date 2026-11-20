#!/usr/bin/env bash
# Mark the worktrees a tutorial will create as trusted by Claude Code, so the
# agent starts without its "Do you trust this folder?" prompt. Claude Code
# asks per folder (a worktree is its own git root, so trusting the project
# above it is not enough); a scripted tutorial knows its task names up front.
#
#   tutorials/worker/trust.sh pinboard "delete notes" "dark theme"
#   tutorials/worker/trust.sh /home/dev/bean-there "landing page"   (any folder)
#
# A task name becomes the worktree folder the way FlightDeck names branches:
# lower case, spaces as dashes ("delete notes" -> delete-notes).
#
# It also marks Claude Code's launch announcements (e.g. "Fable 5.2 is here")
# and notices as seen, puts off its API-credit upsell, and turns off its
# auto-updater (it cannot write the image's npm prefix, and says so in red),
# so they stay out of the video.
set -euo pipefail
NAME="${WORKER_NAME:-fd-tutorial-worker}"
project="$1"; shift
for task in "$@"; do
  slug="$(echo "$task" | tr '[:upper:]' '[:lower:]' | tr ' ' '-')"
  case "$project" in
    /*) echo "$project/.flightdeck/worktrees/$slug" ;;
    *) echo "/home/dev/flightdeck/$project/.flightdeck/worktrees/$slug" ;;
  esac
done | podman exec -i -u dev "$NAME" python3 -c '
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
sp = os.path.expanduser("~/.claude/settings.json")
settings = json.load(open(sp)) if os.path.exists(sp) else {}
settings.setdefault("env", {})["DISABLE_AUTOUPDATER"] = "1"
json.dump(settings, open(sp, "w"), indent=2)
'
