/**
 * Runs before every test file (`test.setupFiles` in `vite.config.ts`).
 *
 * The jsdom suites render a fresh `createApp` per test into one shared
 * document, and the app's keyboard listener is on `document`, guarded only by
 * `frame.isConnected` (see `app.ts`). A frame left in the page keeps answering:
 * by the end of a file every simulated key was reduced and re-rendered by every
 * app the file had made, and the slowest tests ran past vitest's 5 s timeout on
 * CI. Taking each test's frames out of the page afterwards keeps a key's cost
 * to the apps that test made.
 */
import { afterEach } from "vitest";

afterEach(() => {
  if (typeof document !== "undefined") {
    document.body.replaceChildren();
  }
});
