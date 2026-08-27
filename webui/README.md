# FlightDeck Web — `webui/`

The browser control surface for FlightDeck (`specs/WEB_INTERFACE.md`, decision
**D9**). Vite + TypeScript + xterm.js, no framework. Built to static assets and
baked into the desktop binary with `rust-embed` (`src/web/assets.rs`), so a
release stays a single file and the server never resolves paths on disk.

This scaffold does not implement the main screen — that is
`remote-control-sk4u`, built against artboards 1a/1b/1c. What's here is the
seam later tasks extend: the build pipeline, the palette, the font, an
xterm.js instance proven to construct and mount, and a state reducer with
tests (D15).

## Gate commands

This is the third build surface in the repo, alongside the root Rust crate
and the `remote/` cargo workspace — see
`.agents/skills/shipping-flightdeck-changes`. From a clean checkout:

```bash
npm install
npm run build        # -> dist/, which the Rust build embeds
npx tsc --noEmit      # strict typecheck, no emit
npm run test          # vitest — currently just the state reducer (D15)
```

`npm run dev` starts a Vite dev server for iteration; it is not part of the
gate. `npm run test:watch` runs vitest in watch mode.

The Rust side does **not** require `npm run build` to have run — `webui/dist/`
ships a tracked `.gitkeep` so `rust-embed` has a folder to compile against even
on a fresh checkout with no npm build. Without a real build, `assets::lookup`
returns an honest "webui was not built" response instead of a blank page or a
panic. See `src/web/assets.rs`.

## The palette rule

`src/style/tokens.css` ships the **named** semantic palette from
`specs/design/flightdeck-web-turn2.dc.html` artboard `2g — REFERENCE SHEET`,
as CSS custom properties (`--fd-text`, `--fd-stale`, `--fd-term-asleep`, …),
each annotated with the meaning 2g documents. That artboard supersedes the
extracted table in `specs/WEBAPP_DESIGN_BRIEFING_T2.md` §7 — read the artboard
if the two ever disagree.

**Never ship a colour literal for anything the palette already names.** The
rule worth quoting into code review, straight from 2g: *if deleting it would
lose a fact, it cannot be `--fd-text-decor`.* `--fd-text-decor` and
`--fd-text-quiet` look similar on screen and are not interchangeable —
`--fd-text-quiet` is the lifted floor for anything a user could act on
wrongly (`no-upstream`, `git: ?`, an agent's display name); `--fd-text-decor`
is decoration only (key-hint letters, counts, the `│` separator).

## Font

Single family, **JetBrains Mono**, vendored rather than linked from a CDN.
Three static weights (400/500/700, Latin subset, WOFF2 only) are committed
under `public/fonts/`, sourced from the upstream
`@fontsource/jetbrains-mono` npm package (glyphs unmodified, just subsetted;
SIL OFL 1.1, license copied to `public/fonts/JETBRAINS-MONO-LICENSE.txt`). See
`src/style/fonts.css` for the full rationale — in short: this app ships inside
a binary that may be running on a loopback-only or LAN-only host (D1/D5), so a
CDN `@font-face` or a runtime font fetch would silently degrade exactly the
deployment this server exists for. `--fd-font-mono` in `tokens.css` is the one
place code should reach for the family; it falls back to the platform
monospace stack if the vendored font somehow fails to load.

## The no-`FitAddon` invariant

`specs/WEB_INTERFACE.md` D4, as revised by turn 2: **the desktop always owns
PTY geometry**, and the browser *letterboxes* the host's fixed grid — it does
not scale or refit it. `sync_selected_tab_sizes` calls `resize_if_changed`
(`src/lib.rs:5389`) every frame for the selected tab, so any size the browser
claimed for itself would be reverted within one frame.

Concretely: **`@xterm/addon-fit`'s `FitAddon` must never be imported anywhere
in this app.** `src/term/terminal.ts` constructs xterm.js with `cols`/`rows`
taken verbatim from the host and never calls `terminal.resize()` in response
to a container/window resize — only a new geometry value from the host may
resize it. If you find yourself reaching for `FitAddon` to make a terminal
"fill its container", that is exactly the regression D4 turn 2 exists to
prevent; letterboxing (dark margin, not a scaled bitmap) is the intended
behaviour, not a bug to fix.

## Layout

```
webui/
├── index.html          entry point
├── public/fonts/        vendored JetBrains Mono (WOFF2, Latin subset)
├── src/
│   ├── main.ts          scaffold wiring (not the real main screen)
│   ├── state/           the reducer seam — types.ts, reducer.ts, reducer.test.ts
│   ├── term/            xterm.js scaffold (terminal.ts)
│   └── style/           tokens.css (palette + type scale), fonts.css, app.css
├── dist/.gitkeep         tracked placeholder; dist/* itself is gitignored
├── vite.config.ts        also the vitest config (test env: node, no DOM)
└── tsconfig.json         strict
```
