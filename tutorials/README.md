# Tutorial recordings

Scripted screen recordings of FlightDeck Desktop in use, made without a human
at the controls: an agent (or you) drives the real app in a macOS VM against a
real worker, and the recording is cut into a 1080p video with captions,
highlights and an ElevenLabs voiceover.

To make or re-record one with an agent, the project skill
`.claude/skills/recording-flightdeck-tutorials` has the workflow and the
lessons from the first takes.

```
host (Apple Silicon Mac)
├── Tart VM "fd-tutorial"        macOS 26, 1440x810 points (Retina, 2880x1620 px),
│                                scaled up to the 1920x1080 video: a zoomed-in view
│   ├── FlightDeck.app           from target/desktop-dist (scripts/desktop/macos-bundle.sh)
│   └── screencapture -v         records the screen, cursor and clicks
├── VNC daemon (vnc_drive.py)    ONE connection per VM boot; scenes are sent to it
└── Podman "fd-tutorial-worker"  hostname devbox: debug `flightdeck worker`, Claude Code,
                                 the mock projects in tutorials/projects
```

## Run a tutorial

```sh
tutorials/scenes/first-agent.sh          # -> target/tutorials/videos/first-agent.mp4 (+ .vtt)
tutorials/vm/sheet.sh first-agent        # contact sheets, to check a take without watching it
tutorials/publish.sh first-agent         # -> web/public/videos/tutorials/ (web-sized mp4, poster, captions)
```

The scenes (all published on the docs site's Video Tutorials page):

| Scene | Shows |
| --- | --- |
| `intro` | The homepage video: parallel agents, Mission control, session browser, Finish |
| `connect-worker` | `ssh devbox`, `worker status`, `worker pair`, Connect to remote… |
| `first-agent` | The whole loop: project, agent, terminal, session browser, commit, Finish |
| `new-project` | Open a project…: projects, browse, clone, New project; the first agent |
| `parallel-agents` | Three agents at once, Option-1/2/3, three merges |
| `mission-control` | Live tiles, answer from a tile, Earlier today |
| `agent-needs-you` | A waiting agent: sidebar, badges, answering in its terminal |
| `review-changes` | Change count, `git diff`, tests, a revision, Finish |
| `keep-up-to-date` | A teammate's push, `git pull` in a worker shell, target +1, Rebase Worktree |
| `shells` | SSH into a worktree, the Worker tab's shells side by side, Open Worktree in VS Code |
| `files-and-images` | Transfer files…, pasting a mockup into the agent, the result in the browser |
| `web-client` | `worker pair`'s browser link, FlightDeck Web, taken over / Reconnect |
| `worker-setup` | A bare Linux server: the installer, `worker enable`, `worker agents`, `sudo reboot`, back by itself |
| `agent-browser` | Agent: on in the session browser; the agent loads the app, adds a note, checks the count |
| `hooks` | A worktree without node_modules, a `worktree_created` hook in nano, the next worktree runs |
| `terminal-ui` | The terminal UI over SSH: Ctrl-N, two agents, the mode switch, + shell, Ctrl-G Finish |

A take uses real agents (your Claude subscription) and takes 3 to 8 minutes.

A scene script stages everything (`tutorials/prepare.sh`: fresh VM clone,
fresh worker, FlightDeck paired and full screen, about 45 s), records, and
cuts. Raw material is kept next to the video: `NAME-raw.mov` (4K, variable
frame rate) and `NAME.timeline`.

## Pieces

| Command | What it does |
| --- | --- |
| `tutorials/prepare.sh` | Fresh stage, paired and full screen; fails loudly if the Desktop did not attach |
| `tutorials/vm/up.sh` | Fresh VM clone with FlightDeck.app installed; starts the VNC daemon |
| `tutorials/worker/up.sh` | Fresh worker container; prints a pairing code (`SKIP_BUILD=1` reuses the binary) |
| `MODE=noworker\|server tutorials/prepare.sh` | A devbox with no worker: `noworker` for the terminal UI (it refuses next to a worker), `server` for a bare server (systemd as init, FlightDeck not installed, `Containerfile.server`); both stop at the launcher |
| `tutorials/worker/stage-project.sh NAME` | Put `tutorials/stage/NAME` on the worker as a project (git, `origin`), off camera, without adding it to every scene's project list |
| `tutorials/worker/tui-agent-state.sh "task" [state]` | `agent-state.sh` for a terminal-UI scene, where no worker can be asked |
| `tutorials/worker/trust.sh PROJECT "task"...` | Pre-trust the worktrees a scene will create (no Claude Code trust prompt) |
| `tutorials/worker/agent-state.sh "task" [state]` | An agent's state on the worker; scenes `waitfor` it |
| `tutorials/vm/vm shot` | Full-size screenshot plus a 1280 px preview (preview px × 1.125 = screen points) |
| `tutorials/vm/vm run file\|-` | Run scene commands (see `vnc_drive.py`): glide, click, type, key, pause, waitfor |
| `tutorials/vm/vm record start` / `record stop NAME` | Record; stop cuts the video with `edit.py` |
| `tutorials/vm/vm mark cut\|uncut\|fast N\|normal\|caption TEXT` | Editing marks between scene runs |
| `tutorials/vm/terminal.sh` | The Mac's Terminal, filling the screen, font two steps up (rows 18 pt apart) |
| `tutorials/worker/agent-state.sh "task" [state]` | An agent's state; `waitfor` it rather than pausing |
| `tutorials/vm/voice.py "Text"` | Fetch (or reuse) a voiceover clip; `say` in a scene does this itself |

Scene coordinates are screen points (1440x810). Editing marks go into the
timeline with host timestamps; the timeline is plain text, so a take can be
re-cut by editing it and running `edit.py RAW TIMELINE OUT` again.

## Finding things that move

Most positions are fixed screen points, measured from `vm shot` previews. When
a target depends on the take (an agent's answer buttons, a layout that changes
with timing), use `clicktext "Label"` / `highlighttext "Label"`: Vision OCR in
the guest (`vm/guest/find-text.swift`, compiled at boot) finds the text on
screen, the lowest match by default. Or split the scene and decide in bash
from `agent-state.sh`.

## Voiceover and highlights

- `say "Sentence."` in a scene speaks it from that moment (ElevenLabs, voice
  `Gfpl8Yo74Is0W6cPUWWT`, key `ELEVENLABS_API_KEY` in `.env`). Clips are
  cached in `target/tutorials/voice/` by their text, so a re-take or re-cut
  costs nothing for lines already spoken. A script's lines are fetched before
  it runs (off camera); `say` waits for the previous line, and `waitvoice`
  waits for the current one, so the pace follows the narration.
- `highlight X Y W H [spot]` frames a screen area (points) in amber until
  `unhighlight`; `spot` also dims everything else. They fade in and out.
- Captions stay as short step titles; the voice says the fuller sentence.

## One-time setup (already done on this machine)

- Tart from its GitHub release (the Homebrew tap is gone).
- Base image `fd-tutorial-base`: `tart clone ghcr.io/cirruslabs/macos-tahoe-vanilla:26.6.2`,
  then `tart set --cpu 4 --memory 8192`, and in the guest:
  quiet desktop (no widgets, icons, Dock items, sleep, update checks), Command
  Line Tools (git), `192.168.64.1 devbox` in `/etc/hosts`, and Screen
  Recording allowed for `/usr/libexec/sshd-keygen-wrapper` (System Settings,
  clicked over VNC) so `screencapture` works over SSH.
- Python venv `.tutorial-venv` with `vncdotool` and `pillow`.
- Claude Code login in the worker's `fd-tutorial-claude` volume
  (`podman exec -it fd-tutorial-worker claude`, then `/login`). The volume
  also holds the first-run answers and `settings.json`
  (`disableClaudeAiConnectors`, `spinnerTipsEnabled: false`).

## Quirks worth knowing

- **Never connect to the VM's VNC while it records.** Tart's `--vnc-experimental`
  server (Virtualization.framework's private `_VZVNCServer`) crashes the whole
  VM on a client connect during a recording. Hence the daemon. The guest's
  own Screen Sharing is no alternative: it shows "Your screen is being
  controlled" on every connection.
- VNC keys: X11 Alt arrives as Command; Shift is not derived from the keysym,
  so the driver presses it for capitals and symbols. Pause briefly after a
  click before typing, or the first character can be lost.
- Full screen in macOS 26 is View > Enter Full Screen (Fn-F); Ctrl-Cmd-F does nothing.
- Never park the pointer on a screen edge: in full screen the bottom edge
  reveals the Dock.
- The release worker listens on Tailscale only; the tutorial worker is a debug
  build listening via `FLIGHTDECK_WORKER_TEST_LISTEN`, so the Desktop warns
  once that the link is not encrypted (handled in `prepare.sh`).
- macOS's purple recording dot (top right in full screen) is painted out by `edit.py`.
- `tart set --display` alone does not change the guest's resolution: macOS
  keeps the mode it saved. `up.sh` switches it (`vm/guest/display-mode.swift`)
  before the VNC daemon connects, as the VNC framebuffer takes its size then.
- After a failed scene the guest may still be recording: stop it with
  `vm ssh 'pkill -INT screencapture'` and let the next scene boot a fresh VM;
  don't `vnc-restart` into a guest that was recording (Tart crashes).
- The worker: `ssh devbox` from the Mac (port 2222, user `dev`); each project has
  an `origin` in `/srv/git`; FlightDeck Web's files sit at `/src/webui/dist`
  because a debug build reads them from disk; `flightdeck` is wrapped to show
  `devbox:7420` instead of the debug worker's `0.0.0.0`.
- Claude Code in the worker: `settings.json` in the volume also has
  `permissions.additionalDirectories: ["/tmp/flightdeck-paste"]`, so a pasted
  image is read without a permission prompt.
- `screencapture` records a frame only when the screen changes, and after a
  long still stretch it stamps the first new frames too early: a character
  typed after ten quiet seconds showed up ten seconds early. `terminal.sh`
  turns on a blinking cursor (`\e[1 q` in the guest's `.zshrc`) to keep frames
  coming; check a Terminal take for gaps with
  `ffprobe -show_entries frame=pts_time` if something appears before its time.
- `type` text goes through shlex: inside double quotes a backslash stays, so
  write `\$HOME` in an unquoted heredoc (bash makes it `$HOME`), not `\\\$HOME`.
- In a worker window a new agent is not selected while another exists
  (remote-control-bncp): select it with `key opt-N` before typing to it.
- The terminal UI: Mac Terminal sends Shift-Esc as a plain Esc, which would
  interrupt the agent; click the status bar's mode hint for app mode instead.
  Clicking an agent leaves app mode on, so press Enter before typing in a new
  shell. Its New Agent dialog starts on OpenCode (the default backend).
- The Desktop shows a version banner unless the worker's FlightDeck version
  equals the core the app was built with, so build both from the same tree
  (`scripts/desktop/macos-bundle.sh`, `tutorials/worker/build-binary.sh`).
