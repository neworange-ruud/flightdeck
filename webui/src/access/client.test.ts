import { describe, expect, it } from "vitest";
import {
  AUTH_EXCHANGE_PATH,
  AUTH_SESSION_PATH,
  checkSession,
  exchangeCode,
} from "./client";

/**
 * The two HTTP calls, against the bodies `src/web/server.rs` actually sends
 * (`refusal_body`, and the `{ ok: true }` / `{ authenticated: true }` success
 * shapes). Bound to the real contract rather than to a shape invented here, so
 * a change on either side breaks a test instead of a screen.
 */

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("POST /auth/exchange", () => {
  it("sends the code in the body, never in the URL", async () => {
    const calls: { url: string; init?: RequestInit }[] = [];
    await exchangeCode("8412", {
      fetch: async (url, init) => {
        calls.push({ url, ...(init === undefined ? {} : { init }) });
        return jsonResponse(200, { ok: true });
      },
    });

    const call = calls[0];
    expect(call?.url).toBe(AUTH_EXCHANGE_PATH);
    /** The fragment kept it out of the request line on the way in; the body
     * keeps it out of an access log and a `Referer` on the way back. */
    expect(call?.url).not.toContain("8412");
    expect(call?.init?.method).toBe("POST");
    expect(String(call?.init?.body)).toContain("8412");
    /** The cookie the host sets is `HttpOnly`, so this is the whole of the
     * credential handling on the browser side. */
    expect(call?.init?.credentials).toBe("same-origin");
  });

  it("reads a success", async () => {
    const result = await exchangeCode("8412", {
      fetch: async () => jsonResponse(200, { ok: true }),
    });
    expect(result).toEqual({ ok: true });
  });

  it("maps each of the host's four screens", async () => {
    const cases = [
      ["code_entry", "no_code_outstanding"],
      ["rejected", "wrong_code"],
      ["revoked", "token_revoked"],
      ["rate_limited", "rate_limited"],
    ] as const;

    for (const [screen, reason] of cases) {
      const result = await exchangeCode("8412", {
        fetch: async () =>
          jsonResponse(screen === "rate_limited" ? 429 : 401, {
            ok: false,
            screen,
            reason,
            attempts_remaining: 3,
          }),
      });
      expect(result.ok).toBe(false);
      expect(result).toMatchObject({ screen, reason, attemptsRemaining: 3 });
    }
  });

  it("carries the host's attempt budget and lockout, rounded up", async () => {
    const result = await exchangeCode("8412", {
      fetch: async () =>
        jsonResponse(429, {
          ok: false,
          screen: "rate_limited",
          reason: "rate_limited",
          attempts_remaining: 0,
          retry_after_ms: 59_400,
        }),
    });
    /** Whole seconds rounded **up**, matching the server's own `Retry-After`
     * rounding — a countdown that said 0 while the limiter still refused would
     * invite the user to try and be refused again. */
    expect(result).toMatchObject({ attemptsRemaining: 0, lockoutSeconds: 60 });
  });

  it("takes the lockout length and code lifetime from the host, never from a copy", async () => {
    /**
     * These two used to be TypeScript constants mirroring
     * `RATE_LIMIT_LOCKOUT_MS` and `BOOTSTRAP_CODE_TTL_MS`. They are host-sent
     * now: 2b prints the lockout length while the address is *still allowed to
     * try*, which `retry_after_ms` cannot answer because it only exists once
     * the limiter has already fired.
     */
    const result = await exchangeCode("8412", {
      fetch: async () =>
        jsonResponse(401, {
          ok: false,
          screen: "rejected",
          reason: "wrong_code",
          attempts_remaining: 3,
          lockout_seconds: 90,
          code_ttl_seconds: 45,
        }),
    });
    expect(result).toMatchObject({
      lockoutLengthSeconds: 90,
      codeTtlSeconds: 45,
      /** Not the same field: nothing was locked out here. */
      lockoutSeconds: null,
    });
  });

  it("says it does not know rather than remembering the numbers itself", async () => {
    /** A host that sends neither leaves both `null`, and the screens drop the
     * clauses that would have used them. Defaulting to a remembered 60/120
     * would be the browser asserting someone else's constant. */
    const result = await exchangeCode("8412", {
      fetch: async () =>
        jsonResponse(401, { ok: false, screen: "rejected", reason: "wrong_code" }),
    });
    expect(result).toMatchObject({
      lockoutLengthSeconds: null,
      codeTtlSeconds: null,
      attemptsRemaining: null,
    });
  });

  it("dates the revocation on the host's clock, not on this browser's", async () => {
    /**
     * 2b: "withdrew this browser's access **12s ago**". Both timestamps are the
     * host's — subtracting a host instant from `Date.now()` would measure the
     * gap with a clock that may be wrong and print a confident wrong duration
     * on a security screen.
     */
    const result = await exchangeCode("8412", {
      fetch: async () =>
        jsonResponse(401, {
          ok: false,
          screen: "revoked",
          reason: "token_revoked",
          revoked_at_ms: 1_700_000_000_000,
          server_time_ms: 1_700_000_012_000,
        }),
    });
    expect(result).toMatchObject({ screen: "revoked", revokedAgo: "12s ago" });
  });

  it("leaves the revocation undated when the host did not say when", async () => {
    /** Honest degradation: the sentence renders without the clause. Never a
     * fabricated time, and never a zero — which would print as 1970. */
    const result = await exchangeCode("8412", {
      fetch: async () =>
        jsonResponse(401, {
          ok: false,
          screen: "revoked",
          reason: "token_revoked",
        }),
    });
    expect(result).toMatchObject({ screen: "revoked", revokedAgo: null });

    /** And a `revoked_at_ms` with no `server_time_ms` beside it is not enough:
     * there is no host clock to measure it against. */
    const half = await exchangeCode("8412", {
      fetch: async () =>
        jsonResponse(401, {
          ok: false,
          screen: "revoked",
          reason: "token_revoked",
          revoked_at_ms: 1_700_000_000_000,
        }),
    });
    expect(half).toMatchObject({ revokedAgo: null });
  });

  it("falls back to the always-safe screen for a spelling it does not know", async () => {
    const result = await exchangeCode("8412", {
      fetch: async () =>
        jsonResponse(401, { ok: false, screen: "quantum_lockout" }),
    });
    /** A newer host's fifth screen must not render as a blank overlay. Asking
     * for a code is the one answer that is always actionable. */
    expect(result).toMatchObject({ screen: "code_entry" });
  });

  it("does not treat a body that says yes with a 401 as a yes", async () => {
    const result = await exchangeCode("8412", {
      fetch: async () => jsonResponse(401, { ok: true }),
    });
    expect(result.ok).toBe(false);
  });

  it("reports an unreachable host as unreachable, not as a refusal", async () => {
    const result = await exchangeCode("8412", {
      fetch: async () => {
        throw new TypeError("Failed to fetch");
      },
    });
    /**
     * Q7's posture: nobody looked at this code, so "that code did not work"
     * would be a claim we cannot support. The caller shows a connection state
     * instead of an access screen.
     */
    expect(result).toMatchObject({ ok: false, unreachable: true });
  });
});

describe("GET /auth/session", () => {
  it("asks over HTTP rather than opening a socket that would be refused", async () => {
    const calls: string[] = [];
    await checkSession({
      fetch: async (url) => {
        calls.push(url);
        return jsonResponse(200, { authenticated: true });
      },
    });
    expect(calls).toEqual([AUTH_SESSION_PATH]);
  });

  it("accepts the route's own success key", async () => {
    const result = await checkSession({
      fetch: async () => jsonResponse(200, { authenticated: true }),
    });
    expect(result).toEqual({ ok: true });
  });

  it("passes a refusal through with its screen", async () => {
    const result = await checkSession({
      fetch: async () =>
        jsonResponse(401, {
          authenticated: false,
          ok: false,
          screen: "revoked",
          reason: "token_revoked",
          attempts_remaining: 3,
        }),
    });
    expect(result).toMatchObject({ ok: false, screen: "revoked" });
  });
});
