/// <reference types="vitest/config" />
import { writeFileSync } from "node:fs";
import { defineConfig } from "vite";

// `dist/.gitkeep` is a TRACKED placeholder: `rust-embed` walks `webui/dist` at
// Rust compile time, so the directory has to exist in a clean checkout that has
// never run `npm run build` (src/web/assets.rs then reports `NotBuilt` rather
// than failing to compile). `emptyOutDir` deletes it on every build, which would
// leave `git status` permanently showing a deleted tracked file, so put it back
// once the bundle is written.
const keepDistTracked = {
  name: "flightdeck-keep-dist-tracked",
  closeBundle() {
    writeFileSync("dist/.gitkeep", "");
  },
} as const;

// Single Vite config doubling as the vitest config (the `vitest/config` triple-
// slash reference above augments `UserConfig` with the `test` key).
//
// The default environment stays plain `node`: the reducer, the status
// vocabulary, the `Esc Esc` timing and the palette guard are all pure and gain
// nothing from a DOM. The component tests, which render the seven regions of
// artboard 1a and assert on real elements, opt into `jsdom` per file with a
// `@vitest-environment jsdom` docblock — so a DOM is available where the design
// has to be checked, and absent everywhere it would only slow the suite down.
export default defineConfig({
  plugins: [keepDistTracked],
  // `rust-embed` walks this directory at Rust compile time (src/web/assets.rs)
  // and serves it verbatim, so the output must be relocatable: no absolute
  // asset URLs, and a base of "./" keeps `index.html` working when the whole
  // SPA is served from the axum asset route.
  base: "./",
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
