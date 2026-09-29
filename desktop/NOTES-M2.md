# flightdeck-desktop: M2 notes (the Projects-view shell)

Written for beads issues `remote-control-bmej.3.1`, `.3.3`, `.3.4` and `.3.5`.

## Bundled fonts

| Family | Files shipped | Used for |
| --- | --- | --- |
| Geist | Regular (400), Medium (500), SemiBold (600) | all UI text |
| Geist Mono | Regular (400), Medium (500), Bold (700), Italic, BoldItalic | terminals, keycaps, branch names and meta text |

- **Source:** the official release `vercel/geist-font` **v1.7.2**
  (<https://github.com/vercel/geist-font/releases/tag/v1.7.2>), asset
  `geist-font-v1.7.2.zip` (sha256
  `7fc800d2ac6b92844895196e5041aca55d814c15db70c44f79b3b83ab82b04e2`). The
  files are the zip's static cuts, `Geist/ttf/` and `GeistMono/ttf/`,
  unmodified.
- **Licence:** SIL Open Font License 1.1, copyright 2024 The Geist Project
  Authors. The licence text ships next to the fonts as
  `desktop/assets/fonts/OFL.txt`, as the OFL requires for redistribution. The
  OFL allows bundling in any software, including commercial software, as long
  as the fonts are not sold on their own and keep their names. We do not
  rename or modify them.
- **How they load:** `desktop/src/fonts.rs` embeds each file with
  `include_bytes!` and registers them with GPUI's text system
  (`TextSystem::add_fonts`) before the first window opens, so the app renders
  the same on a machine that has never installed Geist. The binary grows by
  about 1.1 MB.
- Only the weights the design uses are shipped. Anything else (a terminal
  program asking for a light weight) is synthesised by the platform from the
  nearest one.
