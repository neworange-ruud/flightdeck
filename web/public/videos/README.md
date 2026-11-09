# Docs videos

Videos for the docs site and landing page. Put them in a docs page with the
`<Video>` component (`web/docs-platform/src/components/mdx-primitives.tsx`):

```mdx
<Video src="/videos/worker-setup.mp4" poster="/videos/worker-setup.jpg"
  captions="/videos/worker-setup.vtt" title="Set up a worker"
  caption="From SSH to a running agent in five minutes." />

<Video youtube="<11-character id>" title="FlightDeck in 90 seconds" />

<Video src="/videos/approve-from-tile.mp4" title="Approve from Mission control" loop />
```

- `title` is required (it labels the player for screen readers).
- Use exactly one of `src` (a file here, or any https URL) or `youtube` (the
  video id, not the URL; it embeds from `youtube-nocookie.com`).
- `loop` turns a short, silent clip into an animated screenshot: muted,
  autoplaying, looping, no controls. Keep those under 15 seconds.
- `poster` (a still shown before playing), `webm` (an extra source) and
  `captions` (a WebVTT file) apply to self-hosted videos only.
- Bad props fail the site build.

## Self-hosted or YouTube?

Everything in this folder is in the git repository and the site's container
image, so keep files here small: loops and clips of a minute or two. Host
anything longer on YouTube (or another CDN) and embed it with `youtube`.

## Recording and encoding

- Record the window at 1280×800 with the demo setup from
  `../screenshots/desktop-app/README.md` (demo repositories, a worker named
  `build-box`, a neutral shell prompt). Before publishing, check every frame
  for hostnames, user names, email addresses, tailnet names, Tailscale `100.x`
  addresses and client projects.
- Encode to H.264 MP4 with the index at the front, so playback starts before
  the whole file has downloaded, and take the poster from the first second:

  ```bash
  ffmpeg -i raw.mov -vf "scale=1280:-2,fps=30,format=yuv420p" \
    -c:v libx264 -crf 26 -preset slow -movflags +faststart -an worker-setup.mp4
  ffmpeg -ss 1 -i worker-setup.mp4 -frames:v 1 -q:v 3 worker-setup.jpg
  ```

  Drop `-an` to keep a voice-over. Aim for under about 1.5 MB per minute.
- Add captions (`.vtt`) when a video has narration.

## Planned videos

| File | Length | Goes on | Shows |
| --- | --- | --- | --- |
| `overview` | 90 s | Landing page, Overview | Two agents start in parallel; one asks for permission, approved from Mission control; Push; the PR link. |
| `desktop-quickstart` | 3 min | Quickstart: Desktop | Download, the macOS first-launch step, open a repository, `Ctrl-n` twice, the sidebar statuses, a notification. |
| `worker-setup` | 5 min | Quickstart: Worker | SSH in, `worker start`, `worker pair`, Connect to remote…, log in from the Worker shell, clone, start an agent, close the laptop, come back. |
| `agent-to-pr` | 3 min | Git Workflow | Commit, Push, open the PR, Pull base after the merge, close or abandon; Finish as the local alternative. |
| `keyboard-flow` | 2 min | The Desktop App | Terminal versus app mode, `Alt-Esc`, the `Ctrl-g` palette, `Alt-↑/↓`, `Alt-m`. |
| `connect-terminal` | 2 min | Quickstart: Worker (optional) | `flightdeck connect` to a worker over SSH. |
