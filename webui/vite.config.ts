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
// slash reference above augments `UserConfig` with the `test` key). D15 only
// asks for `vitest` on the reducer, so the test environment stays plain
// `node` — no jsdom, because the reducer and its tests never touch the DOM.
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
