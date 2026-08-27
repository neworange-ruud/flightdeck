import { defineConfig, devices } from "@playwright/test";

/**
 * The end-to-end job (`specs/WEB_INTERFACE.md` D15) — the only test in this
 * repository that proves the whole chain: PTY bytes → replay ring → protocol
 * frame → WebSocket → what xterm.js actually rendered, and a keystroke back the
 * other way. Everything else (`tests/web_server.rs`, `vitest`) tests one link.
 *
 * ## The flake policy is Q6's, implemented as written
 *
 * - **`retries: 2` in CI, `retries: 0` locally.** Flake has to be *visible*
 *   while developing — a retry on a developer's machine hides exactly the
 *   information they need. In CI two retries buy tolerance for a slow runner
 *   without hiding a test that is genuinely broken (a broken test fails three
 *   times).
 * - **A test that fails twice consecutively on `main` is quarantined the same
 *   working day**, with `test.fixme()` and a filed `bd` issue, rather than left
 *   to erode trust in the suite. The `fixme` carries the issue id, so a
 *   quarantine is never anonymous. See the quarantine block at the bottom of
 *   `e2e/chain.spec.ts` for the exact form.
 * - **Raising `retries` is not an option.** This repository already carries
 *   three iOS flake issues (`remote-control-7lo`, `ba5`, `7lr`) and Q6 exists so
 *   the browser suite does not become a fourth. A flaky test is quarantined and
 *   fixed, never averaged out.
 * - **The job is non-blocking until 2026-09-10, then required.** Registered
 *   `continue-on-error: true` in `.github/workflows/webui.yml`, with the date in
 *   the job's own comment.
 *
 * ## Why serial, single-worker
 *
 * `globalSetup` boots one real FlightDeck (see `e2e/support/host.ts`) and D14
 * gives out exactly one controlling seat. Two workers would fight over it and
 * see takeover screens no test asked for. Serial is a correctness requirement
 * here, not a performance choice.
 */
export default defineConfig({
  testDir: "./e2e",
  /** `.host.json`, the support module and the launcher are not specs. */
  testMatch: /.*\.spec\.ts/,

  globalSetup: "./e2e/global-setup.ts",
  globalTeardown: "./e2e/global-teardown.ts",

  /** Q6: tolerated in CI, visible everywhere else. */
  retries: process.env.CI ? 2 : 0,
  workers: 1,
  fullyParallel: false,

  /** A hung PTY or a socket that never opens fails in a minute, not in
   * Playwright's default half hour of runner time. */
  timeout: 90_000,
  expect: { timeout: 20_000 },

  /** `forbidOnly` in CI: a stray `test.only` would silently shrink the suite to
   * one test and still report green. */
  forbidOnly: !!process.env.CI,

  reporter: process.env.CI
    ? [["list"], ["html", { open: "never" }]]
    : [["list"]],

  use: {
    /**
     * `baseURL` is set per test by the `host` fixture in
     * `e2e/support/fixtures.ts`, because the port is picked at setup time (D5's
     * `[web] port` cannot be 0, so the harness chooses a free one and bakes it
     * into the fixture config).
     */
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "off",
    /**
     * A viewport wide enough that the host's grid fits without the letterbox
     * clipping it — D4 letterboxes rather than scaling, so a small window shows
     * dark margins and, if it were *too* small, would clip. The test asserts on
     * the DOM grid, not on pixels, but this keeps what it asserts visible.
     */
    viewport: { width: 1600, height: 1000 },
  },

  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
