# Desktop app screenshots

Used by the docs (`web/docs/`), the landing page (`web/src/app/page.tsx`, the
hero is `worker-window.png`) and the repository README. To refresh one, replace
the PNG in place with the same name.

All are 1600 px wide, scaled down from the 2560×1602 frames the app renders at
1280×800 on a Retina display. `connect-remote.png` is the separate connect
window, cropped above its saved-remotes list.

| File | Page | Shows |
| --- | --- | --- |
| `worker-window.png` | Overview, Quickstart: Worker, landing page, README | A worker's window: three Claude Code agents (waiting, idle, working), the git strip, a pending permission prompt. |
| `launcher.png` | Quickstart: Desktop, The Desktop App | The launcher with Open project…, Connect to remote…, an online worker and recent projects. |
| `new-agent.png` | Quickstart: Desktop | The New Agent dialog (Ctrl-n) with a task name typed. |
| `mission-control.png` | Quickstart: Desktop, The Desktop App | Mission control on a local window, two tiles with Approve/Deny. |
| `projects-local.png` | The Desktop App | A local Projects view with two agents. |
| `palette.png` | The Desktop App | The command palette (Ctrl-g). |
| `connect-remote.png` | Quickstart: Worker | Connect to remote… with the docs' placeholder address and code. |
| `worker-shell.png` | Quickstart: Worker | The Worker shell tab. |
| `open-project.png` | Quickstart: Worker | A worker's Open a project picker: projects, browse, clone. |
| `../tui/connect.png` | Quickstart: Worker | `flightdeck connect` attached to the same worker. |

## How they were made

Screen capture is not needed. A `--features spike-snapshot` build of
`flightdeck-desktop` renders its own window to an image
(`--spike-snapshot PATH --spike-keys "…"`, see `desktop/NOTES-M0.md`). The set
was rendered against a temporary worker and local window with throwaway `HOME`
directories, demo repositories (`acme-web`, `billing-api`, `docs-site`), a
neutral shell prompt and a saved remote renamed to `build-box`.

**Before committing a retake, check every image for identifying details**:
hostnames, user names, email addresses, tailnet names and Tailscale `100.x`
addresses (a remote window shows the saved address in its titlebar, and shared
dialogs say which address they were opened from), and real client projects.
