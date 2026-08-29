import { readFileSync } from "node:fs";
import { join } from "node:path";

import {
  authenticate,
  describeHost,
  expect,
  renderedTerminal,
  test,
} from "./support/fixtures";
import { PROTOCOL_VERSION } from "../src/wire/frames";

/**
 * The whole chain, end to end (D15).
 *
 * A real FlightDeck is running on a real PTY with a real agent process (see
 * `e2e/support/host.ts`); a real Chromium loads the SPA out of the binary's own
 * asset route and talks the current wire protocol over a real WebSocket. Every
 * assertion below
 * is on something only that arrangement can produce:
 *
 *   PTY bytes → replay ring (D2) → `term_bytes` → WebSocket → xterm.js DOM
 *   keystroke → `input` → PTY stdin → the agent's own reply → back up the chain
 *
 * What this suite deliberately does **not** do is assert on a WebSocket frame.
 * `tests/web_server.rs` already drives 35 tests over real sockets and proves the
 * frames; the only thing left that nothing else can prove is that xterm.js
 * rendered what the PTY emitted, so that is what these tests look at — rendered
 * text in the DOM.
 *
 * Three tests, not ten. Each one is here because it proves a link no unit test
 * can reach.
 */

/** A marker that cannot appear by accident, so finding it proves a round trip. */
function marker(): string {
  return `fd-e2e-${Date.now().toString(36)}`;
}

test.describe("FlightDeck Web, end to end", () => {
  test("serves the embedded SPA and renders what the PTY emitted", async ({
    page,
    request,
    host,
  }) => {
    /**
     * 1. The page comes out of the **binary**, not a dev server.
     *
     * `src/web/assets.rs` serves `webui/dist` through `rust-embed`; a Vite dev
     * server would inject `/@vite/client` and serve `/src/main.ts` as source.
     * Asserting on the hashed bundle name is what distinguishes the two — and it
     * also rules out the "webui was not built" page, which references no bundle
     * at all.
     */
    const index = await request.get(`${host.baseURL}/`);
    expect(index.status()).toBe(200);
    const html = await index.text();
    expect(html).toMatch(/src="\.?\/?assets\/index-[A-Za-z0-9_-]+\.js"/);
    expect(html).not.toContain("/@vite/client");
    expect(html).not.toContain("webui was not built");

    /**
     * 2. Authentication goes through the **real** exchange endpoint.
     *
     * The code is in the URL fragment, the SPA POSTs it to `/auth/exchange`, and
     * the host answers with the `HttpOnly` cookie every later request (the
     * WebSocket included) rides on. The response is caught here rather than
     * inferred from the app's state, so "it authenticated" cannot be satisfied by
     * a client-side shortcut.
     */
    const exchanged = page.waitForResponse(
      (response) =>
        response.url().endsWith("/auth/exchange") &&
        response.request().method() === "POST",
    );
    await page.goto(`${host.baseURL}/#${host.bootstrapCode}`);
    const exchange = await exchanged;
    expect(exchange.status()).toBe(200);
    expect(await exchange.json()).toEqual({ ok: true });

    /** The access overlay is gone: the host let this browser in. */
    await expect(page.locator(".fd-frame")).toHaveAttribute(
      "data-access",
      "false",
    );
    /** And the cookie really is a session, per the host's own answer. */
    const session = await request.get(`${host.baseURL}/auth/session`, {
      headers: {
        cookie: (await page.context().cookies())
          .map((c) => `${c.name}=${c.value}`)
          .join("; "),
      },
    });
    expect(session.status()).toBe(200);
    expect(await session.json()).toMatchObject({ authenticated: true });

    /**
     * 3. **The assertion this whole suite exists for.** `scripts/e2e/fake-agent.sh`
     * printed its banner to its PTY before this browser existed. For it to be in
     * the DOM now it had to be teed into the replay ring, survive as bytes,
     * arrive as a `term_bytes` frame on the WebSocket, and be written into
     * xterm.js — which then had to render it.
     */
    await expect
      .poll(async () => renderedTerminal(page), {
        message: `xterm.js never rendered the agent's banner.\n${describeHost(host)}`,
        timeout: 45_000,
      })
      .toContain("fake-agent: starting");

    /** The connection chip agrees it is a live socket, not a cached paint. */
    await expect(page.locator(".fd-conn")).toContainText("connected");
  });

  test("a keystroke typed in the browser reaches the PTY", async ({
    page,
    host,
  }) => {
    await authenticate(page, host);
    await expect
      .poll(async () => renderedTerminal(page), { timeout: 45_000 })
      .toContain("fake-agent: starting");

    /**
     * Type into the terminal the way a user does: click it (which focuses
     * xterm's hidden textarea) and press keys. Nothing here reaches into the
     * app's state or calls `sendInput` directly — the keystrokes go through
     * xterm's own key handling, which is what owns the translation from a
     * `keydown` to the bytes a PTY expects.
     */
    const typed = marker();
    await page.locator(".fd-mount").click();

    /** The palette chord is captured before xterm can consume it as terminal
     * input. Exercise it with xterm's hidden textarea focused, in real Chromium,
     * because a synthetic event on the app frame cannot prove that ordering. */
    await page.keyboard.press("Control+g");
    await expect(page.locator(".fd-palette")).toBeVisible();
    await page.keyboard.press("Control+g");
    await expect(page.locator(".fd-palette")).toBeHidden();

    await page.keyboard.type(typed);
    await page.keyboard.press("Enter");

    /**
     * The fake agent echoes every line it reads **from stdin** back to stdout.
     * So seeing the marker rendered proves the full round trip: browser →
     * `input` frame → the host wrote it into the right PTY → the agent process
     * read it → its reply came back up the same chain and xterm.js rendered it.
     */
    await expect
      .poll(async () => renderedTerminal(page), {
        message: `the typed marker ${typed} never came back from the PTY.\n${describeHost(host)}`,
        timeout: 45_000,
      })
      .toContain(typed);

    /**
     * And the same fact from outside the browser entirely: the agent appends
     * every line it read to `agent-replies.log`. A file on disk containing the
     * marker cannot be produced by anything the browser rendered locally — the
     * bytes really did reach a process's stdin.
     */
    /**
     * `Esc` is the one key the app takes off xterm (§5: `Esc Esc` leaves focus,
     * a single `Esc` passes through), so it is the one key that goes out through
     * the store's queue. The queue is what 2d's `N keystrokes held` counts, so a
     * transport that did not drain it would leave the pane claiming keystrokes
     * are held that in fact went out — the assertion below is that promise
     * staying true.
     */
    await page.keyboard.press("Escape");
    await expect(page.locator(".fd-pane__held")).toHaveCount(0);

    const log = join(host.fixtureDir, ".flightdeck/agent-replies.log");
    await expect
      .poll(
        () => {
          try {
            return readFileSync(log, "utf8");
          } catch {
            return "";
          }
        },
        {
          message: `${log} never recorded the keystrokes.\n${describeHost(host)}`,
          timeout: 30_000,
        },
      )
      .toContain(typed);
  });

  test("letterboxes the host's grid, and the geometry chip says so (D4)", async ({
    page,
    host,
  }) => {
    await authenticate(page, host);
    await expect
      .poll(async () => renderedTerminal(page), { timeout: 45_000 })
      .toContain("fake-agent: starting");

    /**
     * The host's own numbers, read from the host — not from the app under test.
     *
     * A second, **read-only** socket (D14 allows N observers) asks for a
     * snapshot and reads `geometry` off it. That makes this test non-circular:
     * the expected grid comes from the host's authoritative frame, and the
     * assertions below check that the SPA both rendered and labelled that grid.
     */
    /**
     * The version is passed in from `wire/frames` rather than written as a
     * literal here: the host refuses an `attach` it cannot speak, so a hardcoded
     * number turns every protocol bump into this test timing out on a snapshot
     * that was never coming, twenty seconds at a time.
     */
    const geometry = await page.evaluate<
      { cols: number; rows: number },
      number
    >(
      (protocolVersion) =>
        new Promise((resolve, reject) => {
          const ws = new WebSocket(`ws://${location.host}/ws`);
          const timer = setTimeout(() => {
            ws.close();
            reject(new Error("the observer socket got no snapshot"));
          }, 20_000);
          ws.onopen = () =>
            ws.send(
              JSON.stringify({
                type: "attach",
                protocol_version: protocolVersion,
                seat: "observe",
              }),
            );
          ws.onmessage = (event: MessageEvent) => {
            const frame = JSON.parse(String(event.data)) as {
              type: string;
              geometry?: { cols: number; rows: number };
            };
            if (frame.type === "snapshot" && frame.geometry !== undefined) {
              clearTimeout(timer);
              const { cols, rows } = frame.geometry;
              ws.close();
              resolve({ cols, rows });
            }
          };
          ws.onerror = () => {
            clearTimeout(timer);
            reject(new Error("the observer socket failed to open"));
          };
        }),
      PROTOCOL_VERSION,
    );
    expect(geometry.cols).toBeGreaterThan(0);
    expect(geometry.rows).toBeGreaterThan(0);

    /** The chip names the host's grid, with `×` and the D4 sentence. */
    await expect(page.locator(".fd-gitbar .fd-geometry")).toHaveText(
      `${geometry.cols}×${geometry.rows} · host owns geometry`,
    );

    /**
     * And the grid really is that grid: xterm.js renders one element per row, so
     * the DOM row count is the rendered `rows`. This is the assertion that would
     * catch a `FitAddon` creeping back in — a fitted terminal would size itself
     * to the viewport and stop matching the host.
     */
    await expect(page.locator(".fd-mount .xterm-rows > div")).toHaveCount(
      geometry.rows,
    );
    /** Letterboxed, not scaled: the box that holds the margins exists, and the
     * mount inside it is no wider than the stage around it. */
    const stage = await page.locator(".fd-stage").boundingBox();
    const mount = await page.locator(".fd-mount").boundingBox();
    expect(stage).not.toBeNull();
    expect(mount).not.toBeNull();
    expect(mount!.width).toBeLessThanOrEqual(stage!.width + 1);
    await expect(page.locator(".fd-letterbox")).toHaveCount(1);
  });
});

/**
 * ## Quarantine, per Q6
 *
 * A test that fails **twice consecutively on `main`** is quarantined the same
 * working day — not after a week of arguing about it, and never by raising
 * `retries`. The form, so a quarantine is always attributable:
 *
 * ```ts
 * // QUARANTINED 2026-09-14 — remote-control-xxxx: flakes on the ubuntu runner
 * // when the agent's first banner races the attach. Owner: <who>.
 * test("a keystroke typed in the browser reaches the PTY", async ({ page, host }) => {
 *   test.fixme(true, "remote-control-xxxx: flaky, see the note above");
 *   …
 * });
 * ```
 *
 * `test.fixme` rather than `test.skip` on purpose: `fixme` says "this is
 * expected to be broken and someone owns fixing it", which is what a quarantine
 * is. Every quarantine names a `bd` issue, and the issue is what un-quarantines
 * it. There are no quarantined tests today.
 */
