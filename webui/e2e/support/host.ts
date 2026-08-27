/**
 * Launch a **real** FlightDeck for the end-to-end suite (D15).
 *
 * Nothing here is a stand-in for the host: it builds and runs the actual
 * `flightdeck` binary, on a real PTY, against a real git repository, with the
 * real embedded server listening on a real port. The browser then talks to it
 * over HTTP and a WebSocket exactly as a user's browser would. That is the whole
 * point of this suite — a unit test can prove a frame was encoded correctly, and
 * only this can prove xterm.js rendered what the PTY emitted.
 *
 * The five things that make it hermetic and deterministic:
 *
 * 1. **`--isolated`** gives exactly one session, running the default agent in the
 *    repository root with no worktree and no git mutation (SPECS §32). One
 *    session means one terminal, so `selection.terminal_id` is never ambiguous.
 * 2. **The agent is `scripts/e2e/fake-agent.sh`** — the same deterministic,
 *    network-free stub the Rust E2E harness uses. It prints a known banner on
 *    start and **echoes every line it reads from stdin**, which is what turns "a
 *    keystroke reached the PTY" into something the browser can see come back.
 * 3. **`HOME` is a fresh temp directory**, so `~/.flightdeck` (the global config
 *    and `web.json`, which holds the hashed tokens) is a throwaway. The
 *    developer's own credentials are never read or written.
 * 4. **A fresh fixture repo per run**, with a project `config.toml` that enables
 *    `[web]` on a port this process picked.
 * 5. **`FLIGHTDECK_WEB_TEST_CODE`** — the debug-only seam that keeps a *known*
 *    bootstrap code live, so the browser can authenticate through the real
 *    `POST /auth/exchange`. See `CredentialStore::mint_fixed_bootstrap_code` for
 *    why that is not a credential bypass: the code is still exchanged over the
 *    real endpoint, still expires, is still single use, is still rate limited,
 *    and the method does not exist in a release build.
 *
 * Unix only, for the same reason `tests/remote_e2e.rs` is: the launcher needs a
 * PTY and the GitHub Windows runners have neither that nor bash.
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { createServer } from "node:net";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
/** `webui/e2e/support` → the repository root. */
export const REPO_ROOT = resolve(here, "../../..");
const WEBUI_ROOT = resolve(here, "../..");
/** Where `startHost` records the running host for the spec files to read. */
export const HOST_FILE = join(WEBUI_ROOT, "e2e", ".host.json");

/**
 * The bootstrap code the host is told to keep live. Four digits, as
 * `BOOTSTRAP_CODE_DIGITS` requires; the value itself is arbitrary.
 */
export const BOOTSTRAP_CODE = "8419";

/**
 * The PTY grid the desktop renders into. The host derives the terminal pane's
 * grid from this, and the browser letterboxes whatever the host reports (D4) —
 * so the test never hardcodes the *pane's* geometry, only the window's.
 */
const PTY_ROWS = 40;
const PTY_COLS = 120;

/** Generous: a cold `cargo build` on a CI runner is minutes, not seconds. */
const BUILD_TIMEOUT_MS = 15 * 60 * 1000;
/** First-run global-config seeding, git checks and the PTY spawn all precede
 * the listener. Generous, and a failure prints the PTY transcript. */
const READY_TIMEOUT_MS = 90 * 1000;

export interface HostHandle {
  readonly baseURL: string;
  readonly port: number;
  readonly bootstrapCode: string;
  /** The fixture repository the host is running against. */
  readonly fixtureDir: string;
  /** The throwaway `$HOME` holding its `~/.flightdeck`. */
  readonly homeDir: string;
  /** Everything the desktop painted on its PTY, for diagnosing a failure. */
  readonly logPath: string;
  readonly pid: number;
}

/** Read the handle written by the global setup. */
export function readHostHandle(): HostHandle {
  if (!existsSync(HOST_FILE)) {
    throw new Error(
      `${HOST_FILE} is missing — the Playwright global setup did not run, or it failed before the host came up.`,
    );
  }
  return JSON.parse(readFileSync(HOST_FILE, "utf8")) as HostHandle;
}

async function pickFreePort(): Promise<number> {
  const override = process.env.FD_E2E_PORT;
  if (override !== undefined && override !== "") {
    return Number(override);
  }
  return new Promise<number>((resolvePort, reject) => {
    const probe = createServer();
    probe.once("error", reject);
    probe.listen(0, "127.0.0.1", () => {
      const address = probe.address();
      if (address === null || typeof address === "string") {
        probe.close();
        reject(new Error("could not read a port off the probe socket"));
        return;
      }
      const { port } = address;
      /**
       * `[web] port = 0` is rejected by config validation on purpose (a server a
       * QR code points at needs a stable port), so the port is picked here and
       * baked into the fixture config. The window between closing the probe and
       * the host binding is small but real — that is the honest cost of the
       * config rule, and it is why the ready check reports the port it waited
       * for.
       */
      probe.close(() => resolvePort(port));
    });
  });
}

/** Build the desktop binary unless it is already there and up to date. */
function ensureBinary(): string {
  const binary = join(REPO_ROOT, "target/debug/flightdeck");
  if (process.env.FD_E2E_SKIP_BUILD === "1") {
    if (!existsSync(binary)) {
      throw new Error(
        `FD_E2E_SKIP_BUILD=1 but ${binary} does not exist — run \`cargo build\` first.`,
      );
    }
    return binary;
  }
  execFileSync("cargo", ["build"], {
    cwd: REPO_ROOT,
    stdio: "inherit",
    timeout: BUILD_TIMEOUT_MS,
  });
  if (!existsSync(binary)) {
    throw new Error(`cargo build succeeded but ${binary} is missing`);
  }
  return binary;
}

/**
 * The SPA has to be built, because the assertion is that the page came out of
 * the binary's own asset route (D9) — not out of a Vite dev server. With no
 * build, `src/web/assets.rs` serves its honest "webui was not built" page and
 * the suite would be testing that instead.
 */
function ensureSpaBuilt(): void {
  const index = join(WEBUI_ROOT, "dist/index.html");
  if (!existsSync(index)) {
    throw new Error(
      `${index} is missing — run \`npm run build\` in webui/ first; the E2E suite asserts the page is served from the embedded assets, so there has to be a build to embed.`,
    );
  }
}

function makeFixture(homeDir: string): string {
  const fixtureDir = mkdtempSync(join(tmpdir(), "flightdeck-webui-e2e-repo-"));
  const git = (...args: string[]): void => {
    execFileSync("git", ["-C", fixtureDir, ...args], {
      stdio: "pipe",
      env: { ...process.env, HOME: homeDir },
    });
  };
  git("init", "--quiet", "--initial-branch=main");
  /** Local-only identity, so this works on a runner with no git config and
   * without touching the developer's. */
  git("config", "user.name", "FlightDeck Web E2E");
  git("config", "user.email", "e2e@flightdeck.invalid");
  git("config", "commit.gpgsign", "false");
  writeFileSync(
    join(fixtureDir, "README.md"),
    "# FlightDeck Web E2E fixture\n\nGenerated by webui/e2e/support/host.ts.\n",
  );
  git("add", "README.md");
  git("commit", "--quiet", "-m", "Initial commit");
  return fixtureDir;
}

function writeConfig(fixtureDir: string, port: number): void {
  const fakeAgent = join(REPO_ROOT, "scripts/e2e/fake-agent.sh");
  if (!existsSync(fakeAgent)) {
    throw new Error(`the fake agent stub is missing at ${fakeAgent}`);
  }
  mkdirSync(join(fixtureDir, ".flightdeck"), { recursive: true });
  /**
   * Only the keys this harness cares about. A project-level `[agents.*]` table
   * replaces the global agent set wholesale, which is what makes the run
   * hermetic: `claude` is the only agent, and it is the stub.
   */
  writeFileSync(
    join(fixtureDir, ".flightdeck/config.toml"),
    [
      "[ui]",
      'default_agent = "claude"',
      "",
      "[web]",
      "enabled = true",
      `port = ${port}`,
      'bind = "127.0.0.1"',
      "",
      "[agents.claude]",
      'display_name = "Fake Agent"',
      `command = "${fakeAgent}"`,
      "args = []",
      "",
    ].join("\n"),
  );
}

async function waitForServer(
  baseURL: string,
  child: ChildProcess,
  logPath: string,
): Promise<void> {
  const deadline = Date.now() + READY_TIMEOUT_MS;
  let lastError = "no attempt made yet";
  while (Date.now() < deadline) {
    if (child.exitCode !== null || child.signalCode !== null) {
      throw new Error(
        `the host exited before its server came up (code ${child.exitCode}, signal ${child.signalCode}).\nPTY transcript:\n${tail(logPath)}`,
      );
    }
    try {
      /**
       * `/auth/session` is the cheapest liveness probe that proves the *whole*
       * server is up: it answers 200 or 401, and either is a live axum router.
       */
      const response = await fetch(`${baseURL}/auth/session`, {
        headers: { accept: "application/json" },
      });
      if (response.status === 200 || response.status === 401) {
        return;
      }
      lastError = `HTTP ${response.status}`;
    } catch (error) {
      lastError = String(error);
    }
    await new Promise((r) => setTimeout(r, 250));
  }
  throw new Error(
    `the host's web server never answered at ${baseURL} (last: ${lastError}).\nPTY transcript:\n${tail(logPath)}`,
  );
}

/** Wait until the fake agent has actually started, so the first test does not
 * race the PTY's first bytes. Non-fatal: the specs wait on the DOM anyway. */
async function waitForAgent(fixtureDir: string): Promise<boolean> {
  const status = join(fixtureDir, ".flightdeck/agent-status");
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    if (existsSync(status)) {
      return true;
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  return false;
}

export function tail(logPath: string, bytes = 4000): string {
  if (!existsSync(logPath)) {
    return "(no PTY transcript)";
  }
  const text = readFileSync(logPath, "utf8");
  return text.length > bytes ? text.slice(text.length - bytes) : text;
}

export async function startHost(): Promise<HostHandle> {
  ensureSpaBuilt();
  const binary = ensureBinary();
  const port = await pickFreePort();
  const baseURL = `http://127.0.0.1:${port}`;
  const homeDir = mkdtempSync(join(tmpdir(), "flightdeck-webui-e2e-home-"));
  const fixtureDir = makeFixture(homeDir);
  writeConfig(fixtureDir, port);

  const logPath = join(fixtureDir, "pty.log");
  const log = openSync(logPath, "w");
  const child = spawn(
    "python3",
    [
      join(here, "pty-spawn.py"),
      String(PTY_ROWS),
      String(PTY_COLS),
      binary,
      "--isolated",
    ],
    {
      cwd: fixtureDir,
      env: {
        ...process.env,
        HOME: homeDir,
        TERM: "xterm-256color",
        /** The debug-only seam. Absent from a release build entirely. */
        FLIGHTDECK_WEB_TEST_CODE: BOOTSTRAP_CODE,
        /** Keep the developer's own settings out of it. */
        FLIGHTDECK_REMOTE_AUTOPAIR: "",
      },
      stdio: ["ignore", log, log],
      detached: false,
    },
  );
  child.unref();

  await waitForServer(baseURL, child, logPath);
  await waitForAgent(fixtureDir);

  const handle: HostHandle = {
    baseURL,
    port,
    bootstrapCode: BOOTSTRAP_CODE,
    fixtureDir,
    homeDir,
    logPath,
    pid: child.pid ?? -1,
  };
  writeFileSync(HOST_FILE, `${JSON.stringify(handle, null, 2)}\n`);
  return handle;
}

/** Stop the host and remove its temp directories. Best effort throughout: a
 * teardown that throws would mask the test failure that mattered. */
export function stopHost(): void {
  if (!existsSync(HOST_FILE)) {
    return;
  }
  let handle: HostHandle | null = null;
  try {
    handle = readHostHandle();
  } catch {
    handle = null;
  }
  if (handle !== null && handle.pid > 0) {
    /** SIGTERM reaches the PTY launcher, whose handler SIGKILLs FlightDeck —
     * see `pty-spawn.py` for why the desktop is killed rather than asked. */
    try {
      process.kill(handle.pid, "SIGTERM");
    } catch {
      /* already gone */
    }
  }
  if (handle !== null && process.env.FD_E2E_KEEP_TMP !== "1") {
    for (const dir of [handle.fixtureDir, handle.homeDir]) {
      try {
        rmSync(dir, { recursive: true, force: true });
      } catch {
        /* nothing useful left to do */
      }
    }
  }
  try {
    rmSync(HOST_FILE, { force: true });
  } catch {
    /* nothing useful left to do */
  }
}
