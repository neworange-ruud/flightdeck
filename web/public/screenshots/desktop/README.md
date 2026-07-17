# Desktop screenshots

These images illustrate the desktop-app docs pages. Each file is referenced by
name from the MDX under `web/docs/`, so **to update a screenshot, just replace
the PNG in place with the same filename** — no doc edits needed.

- `main-layout.png` is a **real** capture (the app's main screen).
- Every other file is a **branded placeholder** (dark frame with a monitor
  glyph). Replace each with a real capture when you can.

Aim for a window around **1137×822** (the ratio the placeholders and hero use)
so mixed real/placeholder pages stay visually consistent. A dark terminal theme
matches the docs best.

## What to capture

| File | Page | Capture |
| --- | --- | --- |
| `main-layout.png` | Overview, Interface | ✅ Already provided — the main screen: logo header, project tabs, Agents sidebar with live status, active agent terminal, Git info bar, status bar. |
| `new-tab-agent-menu.png` | Agent Tabs & Worktrees | The agent picker shown after `Ctrl-n` (choose which configured agent to launch). |
| `command-palette.png` | The Interface | The command palette open (`Ctrl-g`), showing the searchable action list. |
| `config-manager.png` | Configuration | The configuration manager (palette → *Open Configuration*), showing settings with their origin labels and the Global/Project header. |
| `git-status.png` | Git Workflow | The *Show Git Status* panel — ideally a worktree that has been pushed so the PR compare URL is visible. |
| `multiple-projects.png` | Multiple Projects | The project tab row with several projects open, showing mixed status dots (red/cyan/dim). |
| `child-terminals.png` | Terminals & Split View | An agent tab with the terminal tab row showing `agent | shell 1 | shell 2` and the `+ agent` / `+ shell` buttons. |
| `split-view.png` | Terminals & Split View | Split view (`Ctrl-b`): the agent terminal and a shell side by side. |


### Optional extras (no placeholder shipped)

Nice-to-have captures not currently shown on any page. Drop the file here **and
add an image reference to the relevant page** if you want them displayed:

- `push-pr-url.png` — the result of a push (`Ctrl-p`): the confirmation and/or
  the GitHub compare URL message. Belongs on the Git Workflow page.
- `containers-doctor.png` — the output of `flightdeck doctor` (Podman readiness
  + per-agent image status). Belongs on the Agents in Containers page.
- `remote-pairing.png` — the desktop *Pair Phone* overlay (QR code, 4-digit
  code, expiry countdown). Would suit the Remote / Pairing docs.
- `help-overlay.png` — the `?` keybindings help overlay. Would suit the
  Interface / Keyboard pages.

## Tips for clean captures

- Use a real Git repo with a few agent tabs in different states (one working,
  one idle, one needing input) so the sidebar looks representative.
- A dark terminal theme with a comfortable font size reads best at doc width.
- Crop to the terminal window (no desktop wallpaper) for a tidy result.

The iOS screenshots (`../mobile/`) are generated automatically from the app in
the Simulator — see `ios/FlightDeckRemoteUITests/DocScreenshotUITests.swift`.
