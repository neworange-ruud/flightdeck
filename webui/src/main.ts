import { consumeBootstrapCode, windowUrlBar } from "./access/bootstrap";
import { checkSession, exchangeCode } from "./access/client";
import type { AccessResult } from "./access/client";
import { fixtureSnapshot } from "./state/fixture";
import { fixtureTerminalBytes } from "./state/fixtureBytes";
import { createApp } from "./ui/app";
import { mountTerminal } from "./term/terminal";
import type { Store } from "./ui/store";
import type { TerminalMount } from "./ui/terminalStage";

/**
 * Entry point.
 *
 * Two halves, and only one of them is real yet:
 *
 *   - **Access is real.** The bootstrap code is consumed from the URL fragment
 *     and stripped from history, exchanged at `POST /auth/exchange`, and
 *     `GET /auth/session` decides between the app and one of artboard 2b's
 *     screens. That whole path talks to the server that exists.
 *   - **The session is still fixture-driven.** `remote-control-hgqy` replaces
 *     the three fixture lines at the bottom with a websocket that dispatches
 *     the same actions — `snapshot/received` from `ServerMsg::Snapshot`,
 *     `connection/changed` from the transport — and swaps `mount` for one that
 *     pipes `ServerMsg::Delta` into `term.write`. The components never learn
 *     the difference.
 */

const root = document.querySelector<HTMLDivElement>("#app");
if (root === null) {
  throw new Error("#app mount point missing from index.html");
}

/**
 * D4: construct xterm with the host's grid and letterbox it. `FitAddon` is
 * absent by design — see `src/term/terminal.ts`.
 */
const mount: TerminalMount = (container, geometry, terminalId) => {
  const term = mountTerminal(container, geometry);
  term.write(fixtureTerminalBytes(terminalId));
  return () => term.dispose();
};

const app = createApp({
  mount,
  /**
   * D3: a selection made here is the whole instance's selection, the desktop
   * included. There is no socket yet, so this is where the `ClientMsg::Command`
   * frame goes — announced rather than silently dropped, so nobody mistakes the
   * fixture for a working remote control.
   */
  onDispatch: (action) => {
    if (action.type.startsWith("selection/")) {
      console.info(
        "[fixture] selection changed locally; ClientMsg::Command { select } goes here (D3)",
        action,
      );
    }
    /** Takeover has no frame of its own: the browser re-sends `Attach`. */
    if (action.type === "takeover/claim" || action.type === "takeover/observe") {
      console.info(
        "[fixture] re-Attach { seat } goes here (D14) — takeover has no dedicated frame",
        action,
      );
    }
  },
  onSubmitCode: (code) => {
    void exchange(app.store, code);
  },
  onStripAction: (action) => {
    if (action.kind === "reload") {
      /** Turn 2 §4: a version mismatch is a stale tab, not a negotiation, and
       * the SPA ships inside the binary (D9) — so the fix really is a reload. */
      window.location.reload();
      return;
    }
    if (action.kind === "code") {
      app.store.dispatch({
        type: "access/required",
        screen: "code_entry",
        attemptsRemaining: null,
        lockoutSeconds: null,
      });
      return;
    }
    /** `retry` belongs to the transport, which `remote-control-hgqy` owns. */
    console.info("[fixture] reconnect now goes here (2c: r Retry now)");
  },
});

root.append(app.el);

/** 2b's footer prints the address the user actually reached, never a guess. */
app.store.dispatch({ type: "host/set", host: window.location.host });

/**
 * Q4, in the order that matters.
 *
 * The fragment is read and **stripped before anything touches the network**.
 * If the strip waited for a successful exchange, every failure — offline, host
 * restarting, code expired, tab closed mid-request — would leave a live
 * credential sitting in the browser's history and in any bookmark made from
 * it. Failing after the strip costs one re-entry on a screen that exists for
 * exactly that; failing before it costs the credential.
 */
void start();

async function start(): Promise<void> {
  const code = consumeBootstrapCode(windowUrlBar());
  if (code !== null) {
    await exchange(app.store, code);
    return;
  }
  /**
   * No code in the URL, so ask whether the cookie we may already hold is
   * still good — over HTTP, not by opening a WebSocket that would be refused.
   * A refused socket would mean a connection error to explain away, and it
   * would spend an attempt against the per-address limiter for nothing.
   */
  applyResult(app.store, await checkSession(), "session");
}

async function exchange(store: Store, code: string): Promise<void> {
  /**
   * `after: "exchange"` matters. A refusal here is a refusal of *digits the
   * user just typed*, so it dispatches `access/refused`, which keeps them for
   * 2b's rejected screen to show — "it expired, or it was mistyped" is only a
   * claim the user can check if the digits are still on screen. A refusal from
   * `GET /auth/session` refused no digits at all.
   */
  applyResult(
    store,
    await exchangeCode(code, { label: navigator.userAgent }),
    "exchange",
  );
}

function applyResult(
  store: Store,
  result: AccessResult,
  after: "exchange" | "session" = "session",
): void {
  if (result.ok) {
    store.dispatch({ type: "access/granted" });
    showFixtureSession();
    return;
  }
  if ("unreachable" in result) {
    /**
     * Nothing refused us — nobody answered. Telling the user "that code did
     * not work" when no one looked at it is exactly the kind of claim Q7
     * forbids, so this is a connection state, not an access screen.
     */
    console.warn("[access] host unreachable:", result.detail);
    store.dispatch({ type: "connection/changed", status: "disconnected" });
    return;
  }
  /**
   * The host chose the screen (`AccessScreen`); the browser renders it.
   *
   * `revoked` gets its own action because it also means the *connection* is
   * revoked — 2c's strip has to say `not allowed` behind the panel, and a
   * plain `access/required` would leave it claiming the link is fine.
   */
  if (result.screen === "revoked") {
    /** An HTTP refusal does not say *when* access was withdrawn, so we do not
     * claim to know: `null` prints the sentence without a time. */
    store.dispatch({ type: "access/revoked", revokedAgo: null });
    return;
  }
  store.dispatch({
    type: after === "exchange" ? "access/refused" : "access/required",
    screen: result.screen,
    attemptsRemaining: result.attemptsRemaining,
    lockoutSeconds: result.lockoutSeconds,
  });
}

/**
 * Everything below here is the fixture, and goes away with
 * `remote-control-hgqy`. It runs only once access is granted, which is also the
 * real sequence: no snapshot exists before a socket is allowed to open.
 */
function showFixtureSession(): void {
  app.store.dispatch({
    type: "snapshot/received",
    snapshot: fixtureSnapshot(),
  });
  app.store.dispatch({ type: "connection/changed", status: "connected" });
  /** 1a is drawn in Terminal mode, so that is the state the fixture shows. */
  app.store.dispatch({ type: "mode/set", mode: "terminal" });
}
