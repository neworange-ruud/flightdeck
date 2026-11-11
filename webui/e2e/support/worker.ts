/**
 * Launch a **real** FlightDeck Worker for the worker spec
 * (`e2e/worker.spec.ts`, `remote-control-vjtb`).
 *
 * The same posture as `./host.ts`: nothing stands in for the worker. This runs
 * the actual `flightdeck worker run` in the foreground, in a throwaway `$HOME`,
 * mints a pairing code with the actual `flightdeck worker pair --json`, and the
 * browser then talks to it over HTTP and a WebSocket exactly as it would over a
 * tailnet. It mirrors how the Rust suite drives a worker
 * (`tests/worker_e2e.rs`), down to the environment:
 *
 * 1. **`HOME` is a fresh directory under `/tmp`, and short.** The worker's
 *    control socket lives in it, and a Unix socket path must fit `sun_path`
 *    (104 bytes on macOS) — `os.tmpdir()` on macOS is far too deep.
 * 2. **`FLIGHTDECK_WORKER_TEST_LISTEN=127.0.0.1:0`** — the debug-only seam
 *    (`src/worker/run.rs`) that makes a worker listen on loopback instead of
 *    its Tailscale addresses. A release build ignores it entirely; a test has
 *    no tailnet. The bound port is read back from `worker pair --json`.
 * 3. **`SHELL=/bin/sh`**, so the worker shell is a known, plain shell.
 * 4. **A global config with the fake agent** (`scripts/e2e/fake-agent.sh`) as
 *    the default, update checks off, and the projects root at
 *    `~/flightdeck`, where the fixture repository is created — so the picker's
 *    scan finds it.
 * 5. **No system or user git config**, and a fixed identity, so git behaves
 *    the same on every machine.
 *
 * The worker serves the SPA from its own binary, so `webui/dist` must be built
 * and the binary must be one whose worker serves assets.
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";

import { REPO_ROOT } from "./host";

/** A cold `cargo build` on a CI runner is minutes. */
const BUILD_TIMEOUT_MS = 15 * 60 * 1000;
/** Worker start-up: config load, the shell's spawn, the listener. */
const READY_TIMEOUT_MS = 60 * 1000;

/** The fixture repository's folder name, under the projects root. */
export const FIXTURE_REPO = "webui-fixture";

export interface WorkerHandle {
  /** `http://127.0.0.1:<port>` — where the worker serves the SPA. */
  readonly baseURL: string;
  /** The throwaway `$HOME`. */
  readonly homeDir: string;
  /** The fixture repository, at `~/flightdeck/webui-fixture`. */
  readonly repoDir: string;
  /** The worker's stderr, for diagnosing a failure. */
  readonly logPath: string;
  readonly child: ChildProcess;
}

/** Built once per run: `pair` asks for it every time it mints a code. */
let built: string | null = null;

function binary(): string {
  if (built !== null) {
    return built;
  }
  const path = join(REPO_ROOT, "target/debug/flightdeck");
  built = path;
  if (process.env.FD_E2E_SKIP_BUILD === "1") {
    if (!existsSync(path)) {
      throw new Error(
        `FD_E2E_SKIP_BUILD=1 but ${path} does not exist — run \`cargo build\` first.`,
      );
    }
    return path;
  }
  execFileSync("cargo", ["build"], {
    cwd: REPO_ROOT,
    stdio: "inherit",
    timeout: BUILD_TIMEOUT_MS,
  });
  return path;
}

/** The environment every worker command runs with — `tests/worker_e2e.rs`'s
 * `command()` and `git_env()`. */
function workerEnv(homeDir: string): NodeJS.ProcessEnv {
  return {
    ...process.env,
    HOME: homeDir,
    SHELL: "/bin/sh",
    FLIGHTDECK_WORKER_TEST_LISTEN: "127.0.0.1:0",
    FLIGHTDECK_REMOTE_AUTOPAIR: "",
    GIT_AUTHOR_NAME: "FlightDeck Web E2E",
    GIT_AUTHOR_EMAIL: "e2e@flightdeck.invalid",
    GIT_COMMITTER_NAME: "FlightDeck Web E2E",
    GIT_COMMITTER_EMAIL: "e2e@flightdeck.invalid",
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_TERMINAL_PROMPT: "0",
  };
}

function writeGlobalConfig(homeDir: string): void {
  const fakeAgent = join(REPO_ROOT, "scripts/e2e/fake-agent.sh");
  if (!existsSync(fakeAgent)) {
    throw new Error(`the fake agent stub is missing at ${fakeAgent}`);
  }
  mkdirSync(join(homeDir, ".flightdeck"), { recursive: true });
  writeFileSync(
    join(homeDir, ".flightdeck/config.toml"),
    [
      "[ui]",
      'agent_tab_position = "left"',
      'default_agent = "fake"',
      "",
      "[update]",
      "check = false",
      "",
      "[worker]",
      'projects_root = "~/flightdeck"',
      "",
      "[agents.fake]",
      'display_name = "Fake"',
      `command = "${fakeAgent}"`,
      "args = []",
      "",
    ].join("\n"),
  );
}

function makeRepo(homeDir: string, dir: string): void {
  mkdirSync(dir, { recursive: true });
  const git = (...args: string[]): void => {
    execFileSync("git", ["-C", dir, ...args], {
      stdio: "pipe",
      env: workerEnv(homeDir),
    });
  };
  git("init", "--quiet", "--initial-branch=main");
  git("config", "commit.gpgsign", "false");
  writeFileSync(join(dir, "README.md"), "# FlightDeck worker E2E fixture\n");
  git("add", "README.md");
  git("commit", "--quiet", "-m", "Initial commit");
}

interface PairingCode {
  readonly code: string;
  readonly address?: string | null;
  readonly listening: boolean;
}

/** `flightdeck worker pair --json`: a fresh four-digit code, and where the
 * worker listens. */
export function pair(worker: { readonly homeDir: string }): PairingCode {
  const out = execFileSync(binary(), ["worker", "pair", "--json"], {
    cwd: worker.homeDir,
    env: workerEnv(worker.homeDir),
    stdio: ["ignore", "pipe", "pipe"],
  });
  return JSON.parse(out.toString("utf8")) as PairingCode;
}

export function tail(path: string, bytes = 4000): string {
  if (!existsSync(path)) {
    return "(no worker log)";
  }
  const text = readFileSync(path, "utf8");
  return text.length > bytes ? text.slice(text.length - bytes) : text;
}

/** Start `worker run` and wait until it listens and serves the SPA. */
export async function startWorker(): Promise<WorkerHandle> {
  if (!existsSync(join(REPO_ROOT, "webui/dist/index.html"))) {
    throw new Error("webui/dist is missing — run `npm run build` in webui/ first.");
  }
  const flightdeck = binary();
  /** `/tmp`, not `os.tmpdir()`: see point 1 in the module note. */
  const homeDir = mkdtempSync("/tmp/fdw-web-");
  writeGlobalConfig(homeDir);
  const repoDir = join(homeDir, "flightdeck", FIXTURE_REPO);
  makeRepo(homeDir, repoDir);

  const logPath = join(homeDir, "worker-run.stderr");
  const log = openSync(logPath, "w");
  const child = spawn(flightdeck, ["worker", "run"], {
    cwd: homeDir,
    env: workerEnv(homeDir),
    stdio: ["ignore", "ignore", log],
  });

  const deadline = Date.now() + READY_TIMEOUT_MS;
  let lastError = "no attempt yet";
  while (Date.now() < deadline) {
    if (child.exitCode !== null || child.signalCode !== null) {
      throw new Error(
        `the worker exited early (code ${child.exitCode}, signal ${child.signalCode}):\n${tail(logPath)}`,
      );
    }
    try {
      const code = pair({ homeDir });
      const address = code.address ?? null;
      if (code.listening && address !== null) {
        const baseURL = `http://${address}`;
        const page = await fetch(`${baseURL}/`);
        if (page.ok && (await page.text()).includes('id="app"')) {
          return { baseURL, homeDir, repoDir, logPath, child };
        }
        lastError = `GET / answered ${page.status} without the SPA — is this a worker that serves assets?`;
      } else {
        lastError = "not listening yet";
      }
    } catch (error) {
      lastError = String(error);
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  child.kill("SIGKILL");
  throw new Error(`the worker never served the SPA (last: ${lastError}):\n${tail(logPath)}`);
}

/** SIGTERM the worker, wait for it, and remove its home. Best effort. */
export async function stopWorker(worker: WorkerHandle | null): Promise<void> {
  if (worker === null) {
    return;
  }
  const { child } = worker;
  if (child.exitCode === null && child.signalCode === null) {
    const exited = new Promise<void>((resolve) => child.once("exit", () => resolve()));
    child.kill("SIGTERM");
    await Promise.race([
      exited,
      new Promise<void>((resolve) => setTimeout(resolve, 10_000)),
    ]);
    if (child.exitCode === null && child.signalCode === null) {
      child.kill("SIGKILL");
    }
  }
  if (process.env.FD_E2E_KEEP_TMP !== "1") {
    try {
      rmSync(worker.homeDir, { recursive: true, force: true });
    } catch {
      /* nothing useful left to do */
    }
  }
}
