import { test as base, type Page } from "@playwright/test";

import { readHostHandle, tail, type HostHandle } from "./host";

/**
 * The running host, handed to every test, plus `baseURL` pointed at it.
 *
 * `baseURL` has to be a fixture rather than a config constant because the port
 * is chosen while the host starts (`[web] port = 0` is rejected by config
 * validation, so the harness picks a free one and bakes it into the fixture
 * config it writes).
 */
export const test = base.extend<{ host: HostHandle }>({
  host: async ({}, use) => {
    await use(readHostHandle());
  },
  baseURL: async ({}, use) => {
    await use(readHostHandle().baseURL);
  },
});

export { expect } from "@playwright/test";

/**
 * Authenticate this browser context the way a user does: open the page with the
 * bootstrap code in the URL **fragment** (never a query string — a fragment is
 * not sent to the server, so it cannot land in a log), let the SPA POST it to
 * `/auth/exchange`, and wait until the access overlay is gone.
 *
 * Every test does this from a fresh context, so every test exercises the real
 * exchange rather than reusing a saved cookie.
 */
export async function authenticate(page: Page, host: HostHandle): Promise<void> {
  await page.goto(`${host.baseURL}/#${host.bootstrapCode}`);
  const frame = page.locator(".fd-frame");
  await frame.waitFor();
  /** `data-access="false"` is the app saying the host let us in. */
  await frame.and(page.locator('[data-access="false"]')).waitFor();
}

/** Everything xterm.js has rendered into the DOM, as text. */
export async function renderedTerminal(page: Page): Promise<string> {
  return page.locator(".fd-mount .xterm-rows").innerText();
}

/** Attach the desktop's PTY transcript to a failure, so a red run in CI can be
 * read without re-running it locally. */
export function describeHost(host: HostHandle): string {
  return `host ${host.baseURL} (pid ${host.pid})\nPTY transcript:\n${tail(host.logPath)}`;
}
