---
name: recording-flightdeck-tutorials
description: Use when making, re-recording, fixing or publishing a FlightDeck tutorial video, such as a new narrated screencast of FlightDeck Desktop driving a worker, a re-take after a UI change or release, a re-cut of a take (captions, highlights, voiceover, speed-ups), or adding a video to the docs site's Video Tutorials page or the homepage intro. Also use when the user asks for a demo, walkthrough or screen recording of FlightDeck, even if they don't say "tutorial", and before touching anything in tutorials/ (scenes, vm/, worker/, edit.py, publish.sh).
---

# Recording FlightDeck tutorials

The videos on the docs site's Video Tutorials page (`web/docs/tutorials.mdx`)
and the homepage intro are recorded with no one at the controls. A bash
**scene script** (`tutorials/scenes/NAME.sh`) does four things:

1. It stages a fresh macOS VM with FlightDeck Desktop and a fresh worker
   container (`devbox`).
2. It drives the real app over VNC while real Claude Code agents work.
3. It writes editing marks (captions, highlights, speed-ups, voice lines) to a
   timeline.
4. It cuts the raw recording into a narrated 1080p video.

`tutorials/README.md` is the reference for every command, the one-time setup
and the platform quirks. Read it first. This skill covers the workflow and the
lessons learned while making the first twelve videos, which the README states
only briefly or not at all.

## The workflow

1. **Plan the story.** One task, start to finish, 45 to 110 s. Write it as
   beats: what the viewer sees, plus one spoken sentence per beat. Keep
   captions as short step titles ("Answer from the tile"). The voice says the
   full sentence.
2. **Write the scene** by copying the closest existing one.
   - `mission-control.sh` is short and fixed-layout.
   - `intro.sh` uses OCR targeting and two `vm run` blocks.
   - `files-and-images.sh` and `shells.sh` show Terminal and file work.

   The usual structure:
   ```bash
   "$ROOT/tutorials/prepare.sh"                       # fresh VM + worker, paired, full screen
   "$ROOT/tutorials/worker/trust.sh" pinboard "$TASK" # pre-trust the worktrees agents will get
   "$VM" run - <<EOF   # off camera: open the project, start agents, wait for state
   ...
   EOF
   "$VM" record start
   "$VM" run - <<EOF   # on camera: caption / say / glide / click / waitvoice ...
   ...
   caption
   pause 0.6
   cut
   EOF
   "$VM" record stop NAME   # -> target/tutorials/videos/NAME.mp4 (+ .vtt, -raw.mov, .timeline)
   ```
3. **Measure coordinates** you don't have yet.
   - `references/ui-coordinates.md` has the known ones.
   - For new targets, stage once with `tutorials/prepare.sh`, take a
     `tutorials/vm/vm shot` and read the 1280 px preview. Multiply preview
     pixels by 1.125 to get screen points.
   - If a target moves between takes, don't use a coordinate at all: use
     `clicktext` or `highlighttext` (see below).
4. **Record**: `tutorials/scenes/NAME.sh`.
   - A take takes 3 to 8 minutes and uses the user's Claude subscription.
   - Run it in the background and wait for it to finish rather than polling.
5. **Review the take** before showing it to anyone.
   - Run `tutorials/vm/sheet.sh NAME` to get contact sheets and look at them.
   - For any doubtful moment, extract a frame
     (`ffmpeg -ss T -i …/NAME.mp4 -frames:v 1 x.png`) and check it. Look for:
     - highlights that miss their target;
     - a dialog or notification covering the action;
     - a click that didn't land, so the next beat shows the wrong screen;
     - narration running past the end of the video;
     - a caption that contradicts the screen.
6. **Fix it.**
   - If the problem is timing, a highlight box, or a caption, re-cut without
     re-recording. Edit `target/tutorials/videos/NAME.timeline` (plain text,
     host timestamps), then run
     `.tutorial-venv/bin/python tutorials/vm/edit.py NAME-raw.mov NAME.timeline NAME.mp4`
     from that folder.
   - If the problem is in the action itself, fix the scene and take it again.
     Voice clips are cached by their text, so a re-take only pays ElevenLabs
     for lines that changed.
7. **Publish**: `tutorials/publish.sh NAME`. Pass `POSTER=secs` to pick a
   poster frame that shows the point of the video.
   - It writes `web/public/videos/tutorials/NAME.{mp4,jpg,vtt}` and prints the
     duration.
   - The videos are kept in the repo, at about 1.3 MB a minute.
8. **Put it on the site.** See "Adding a video to the site" below.

## Writing scenes that work on the first take

These rules come from takes that failed. The reason is given with each one, so
you can judge a case none of them covers.

- **Set up off camera.** Everything the viewer doesn't need to see goes
  before `record start`: opening the project, starting a background agent,
  waiting for an agent to reach the state the story starts from. A video that
  opens on the point is worth more than one that shows every click.
- **Wait on state, not on time.** Agents are nondeterministic.
  - Use `waitfor SECS $STATE "task" waiting|working|idle`, which runs
    `tutorials/worker/agent-state.sh`, instead of a `pause` sized from one lucky
    run.
  - Wrap long agent work in `fast 6` … `normal`, so the viewer sees it happen,
    sped up, with a "6x" badge.
- **Make the layout settle before you click into it.**
  - Mission control's tiles reflow as agents change state. A click aimed at an
    answer button once landed on a Finish card instead.
  - When a beat clicks inside a layout that depends on agent state, first wait
    until the other agents are in a stable state (for example, `idle`).
- **Find moving targets by their text.**
  - `clicktext "Oldest first"` and `highlighttext "Label" [N] [PAD]` OCR the
    guest's screen. They pick the lowest match by default; N picks another
    (top to bottom, -1 is the lowest).
  - Use them for anything an agent or the timing places: answer buttons,
    dialogs, list rows.
- **Pace by the voice.**
  - `say` starts a line, and it first waits for the previous line to finish.
  - `waitvoice [extra]` waits for the current line to end.
  - Put the action for a line between its `say` and its `waitvoice`. The
    narration and the screen then stay together however long either takes.
  - Every scene's last line needs a `waitvoice` before the closing
    `caption` / `pause` / `cut`.
- **Highlight what the voice names.**
  - Bracket the line with `highlight X Y W H` … `unhighlight`. Add `spot` to
    dim everything else when the screen is busy.
  - Pad boxes by about 8 pt.
  - Check them in a frame. They are the most common re-cut.
- **Write agent prompts that end predictably.**
  - Name the files the agent may touch.
  - End with "Commit your work when you're done."
  - To get a waiting agent, ask it to "ask me … first, as a multiple-choice
    question" before it changes anything.
  - Pre-trust every task name with `trust.sh`, or Claude Code's trust prompt
    takes over the take.
- **Keys go through the driver's names.**
  - `cmd-` is Command and `opt-` is Option (`key opt-1`).
  - It is `esc`, not `escape`. The driver rejects unknown names before
    anything runs.
  - After a click, pause about 0.3 s before typing, or the first character can
    get lost.
- **Use `paste` off camera and `type TEXT CPS` on camera.** The viewer should
  see typing at a human pace (about 45 cps for long prompts, 18 for names).
- **Never park the pointer on a screen edge.** The bottom edge brings up the
  Dock in full screen. Park it around 760,420 between beats.
- **Use the Mac's Terminal for host-side shells.** `tutorials/vm/terminal.sh`
  opens a fresh window filling the screen. Rows are 18 pt apart, the first
  at y 83. Close it with `killall Terminal`: Quit stops at a confirmation
  dialog.

## When a take fails

- **A scene step errors out.** The guest may still be recording. Run
  `tutorials/vm/vm ssh 'pkill -INT screencapture'`, fix the scene, and run the
  whole scene again: `prepare.sh` boots a fresh VM.
- **Don't reconnect VNC.** Never `vm vnc-restart` into a guest that was
  recording, and never open your own VNC client while it records. Tart's VNC
  server crashes the VM.
- **Long-lived host processes die with the task that started them.** This
  applies to the VNC daemon, the VM, and anything else `lib.sh` starts. They
  go through `tutorials/vm/detach`. If you add one, use `$DETACH` too.
- **A banner or notification shows up in the take.** Find what produced it
  and switch it off in the stage, not in the scene.
  - Examples: an "Agent finished" notification, Claude Code announcements, a
    version-skew banner.
  - `prepare.sh`, `worker/trust.sh` and the worker's config are the places.
  - A version banner means the Desktop app and the worker binary were built
    from different trees. Rebuild both with `scripts/desktop/macos-bundle.sh`
    and `tutorials/worker/build-binary.sh`.
- **FlightDeck itself misbehaves.** That's a finding, not a scene problem.
  File a bd issue (labels `area:desktop` and/or `area:worker`, one
  `bd label add` per label), work around it in the scene if you can, and tell
  the user.

## Re-recording for a new release

1. Rebuild the app and the worker from the release tree (see the version
   banner above).
2. Run each scene.
3. Check each take (step 5 of the workflow).
4. Publish it.
5. Update the durations in the docs page's Contents list.

UI coordinates may have moved. Before you take every scene, stage once and
compare a `vm shot` against `references/ui-coordinates.md`.

## Adding a video to the site

1. **The tutorials page.** In `web/docs/tutorials.mdx`:
   - Add a `### Title` under the right `##` group (Get started, Work with
     agents, The worker).
   - Add the video:
     `<Video src="/videos/tutorials/NAME.mp4" poster="/videos/tutorials/NAME.jpg" captions="/videos/tutorials/NAME.vtt" title="Title" />`.
   - Add two or three sentences on what it shows, with a "More in [Page](/page)"
     link. Docs links are written as `/page`, and the site puts them under
     /docs.
   - Add an entry to the **Contents** list at the top, with the duration as
     m:ss.
2. **The homepage.** The intro video plays straight from `intro.mp4`, so
   re-publishing `intro` updates the homepage.
3. **The tutorials README.** Add a row to the scene table in
   `tutorials/README.md`.
4. **The changelog.** Add a `CHANGELOG.md` entry under `[Unreleased]` when the
   change goes into a PR (the shipping-flightdeck-changes skill covers the
   rest of the PR).
5. **Check it locally.** `cd web && npm run build && npx next start -p 3100`
   (port 3000 is often taken). Open /docs/tutorials, play the video, and
   switch the captions on.
6. **Deploy.** The site deploys by itself only with a CLI release tag. To
   publish sooner, merge first, then run
   `gh workflow run web-deploy.yml --ref main`.
   - The workflow reports success before the new revision serves traffic.
   - Check the live page itself, and compare the video's size with the local
     file.
