/**
 * Fixture PTY bytes — what the host *would* be streaming.
 *
 * These are raw terminal bytes with plain SGR escapes, deliberately not
 * pre-coloured HTML: D2 says the browser receives raw PTY bytes and lets
 * xterm.js parse them, so the fixture has to arrive the same way the wire will
 * or the render path would go untested. `remote-control-hgqy` deletes this
 * file and pipes `ServerMsg::Delta` payloads into `Terminal.write` instead —
 * nothing else changes.
 *
 * Colour is expressed as ANSI *indices* (31 red, 32 green, 36 cyan, …), never
 * as truecolour triples, because the index-to-token mapping belongs to the
 * xterm theme in `src/term/terminal.ts`, which reads it from `tokens.css`.
 * That is the same reason there is no hex value anywhere in this file.
 *
 * Content is transcribed from artboard 1a's viewport (the agent transcript)
 * and 1c's two shell columns.
 */

const RESET = "\x1b[0m";
const BOLD = "\x1b[1m";
const RED = "\x1b[31m";
const GREEN = "\x1b[32m";
const YELLOW = "\x1b[33m";
const CYAN = "\x1b[36m";
const WHITE = "\x1b[37m";
const DIM = "\x1b[90m";

const agentTranscript: readonly string[] = [
  `${GREEN}●${RESET} Read the whole spec folder. Starting on the relay handshake.`,
  "",
  `${GREEN}●${RESET} ${BOLD}Bash${RESET}(cargo test -p flightdeck-relay -- --nocapture)`,
  `  ${DIM}⎿${RESET} ${WHITE}   Compiling flightdeck-relay v1.16.0 (/Users/dev/flightdeck/relay)${RESET}`,
  `     ${WHITE}    Finished test profile in 4.81s${RESET}`,
  `     ${WHITE}     Running unittests src/lib.rs${RESET}`,
  `     ${GREEN}test handshake::pair_code_expires_after_120s ... ok${RESET}`,
  `     ${GREEN}test handshake::rejects_replayed_nonce ...... ok${RESET}`,
  `     ${RED}test session::viewer_fanout_preserves_order . FAILED${RESET}`,
  `  ${DIM}⎿ Allowed by auto mode classifier${RESET}`,
  "",
  `${GREEN}●${RESET} The fan-out test fails because ${CYAN}relay/src/session.rs:214${RESET} writes to viewers`,
  "  from a HashMap iteration, so ordering is non-deterministic once a second",
  `  browser attaches. Switching to an insertion-ordered ${YELLOW}Vec<ViewerHandle>${RESET}.`,
  "",
  `${GREEN}●${RESET} ${BOLD}Edit${RESET}(relay/src/session.rs)`,
  `  ${DIM}⎿${RESET} ${GREEN}+  viewers: Vec<ViewerHandle>,${RESET}`,
  `     ${RED}-  viewers: HashMap<ViewerId, ViewerHandle>,${RESET}`,
  `     ${WHITE}   18 additions, 11 removals across 2 files${RESET}`,
  "",
  `${GREEN}●${RESET} Re-running the suite, then I will push ${CYAN}flightdeck/fix-login-redirect${RESET}.`,
  "",
  `${YELLOW}✻${RESET} ${WHITE}Churning for 2m 14s · esc to interrupt${RESET}`,
  "",
  `${CYAN}›${RESET} `,
  `  ${DIM}▸▸ auto mode on (shift+tab to cycle) · ← for agents${RESET}`,
];

const shellOne: readonly string[] = [
  `${GREEN}➜${RESET} ${CYAN}fix-login-redirect${RESET} pnpm test:e2e`,
  ` ${DIM}RUN${RESET}  v2.1.4 /worktrees/fix-login`,
  "",
  ` ${GREEN}✓${RESET} auth/session.spec.ts  ${DIM}(11)${RESET}`,
  ` ${GREEN}✓${RESET} auth/redirect.spec.ts ${DIM}(7)${RESET}`,
  ` ${RED}✗${RESET} auth/logout.spec.ts   ${DIM}(3)${RESET}`,
  `   ${RED}→ expected 302, got 200${RESET}`,
  "",
  ` Tests  ${RED}1 failed${RESET} | ${GREEN}20 passed${RESET}`,
  `${GREEN}➜${RESET} ${CYAN}fix-login-redirect${RESET} `,
];

const shellTwo: readonly string[] = [
  `${GREEN}➜${RESET} ${CYAN}fix-login-redirect${RESET} git log --oneline -6`,
  `${YELLOW}a19c4f2${RESET} fix(auth): honour returnTo`,
  `${YELLOW}7b2e8d0${RESET} test: cover logout redirect`,
  `${YELLOW}3c9a114${RESET} refactor: viewer fan-out`,
  `${DIM}e40b7aa${RESET} chore: bump relay deps`,
  `${DIM}9d1f003${RESET} docs: relay handshake notes`,
  `${DIM}55ce8b1${RESET} feat: pair-code expiry`,
  "",
  `${GREEN}➜${RESET} ${CYAN}fix-login-redirect${RESET} `,
];

/**
 * Bytes for one terminal. Unknown ids get an honest placeholder rather than a
 * blank screen — a terminal the host has not streamed yet is a fact worth
 * showing, exactly like `git: ?`.
 */
export function fixtureTerminalBytes(terminalId: string): string {
  const lines =
    terminalId === "t-shell-1"
      ? shellOne
      : terminalId === "t-shell-2"
        ? shellTwo
        : terminalId.endsWith("agent") || terminalId === "t-agent"
          ? agentTranscript
          : [`${DIM}no bytes streamed for ${terminalId} yet${RESET}`];

  return lines.join("\r\n");
}
