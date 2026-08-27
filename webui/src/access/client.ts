import { parseAccessScreen } from "../state/access";
import type { AccessScreen } from "../state/model";

/**
 * The two HTTP calls the access screens make (Q4, D5).
 *
 * Both are the real routes the axum server serves, named here so the SPA and
 * `src/web/server.rs` cannot drift on a string:
 *
 * | Route | Question it answers |
 * | --- | --- |
 * | `GET /auth/session` | is my cookie still good? |
 * | `POST /auth/exchange` | here is a bootstrap code — give me a cookie |
 *
 * `GET /auth/session` exists so the SPA can choose between the app and 2b's
 * code screen **without opening a WebSocket it will be refused**. A refused
 * socket would mean a connection error to explain away, and it would spend an
 * attempt against the per-address limiter for nothing.
 *
 * Neither call ever handles a token. The credential is an `HttpOnly` cookie
 * (`flightdeck_web`), which means JavaScript in this app *cannot* read it —
 * that is the point of `HttpOnly`, and it is why `credentials: "same-origin"`
 * below is the whole of the auth logic on the browser side.
 *
 * `fetch` is injected so the tests are plain unit tests with no server.
 */

export const AUTH_SESSION_PATH = "/auth/session";
export const AUTH_EXCHANGE_PATH = "/auth/exchange";

/** A refusal, exactly as `refusal_body()` in `src/web/server.rs` sends it. */
export interface AccessRefusal {
  readonly ok: false;
  /** Which of 2b's screens to show — the host's decision, not ours. */
  readonly screen: AccessScreen;
  /** The host's stable spelling (`wrong_code`, `token_revoked`, …), for logs. */
  readonly reason: string;
  /** `CredentialStore::attempts_remaining`, or `null` if absent. */
  readonly attemptsRemaining: number | null;
  /** From `AuthFailure::RateLimited { retry_after_ms }`, in whole seconds. */
  readonly lockoutSeconds: number | null;
}

export type AccessResult =
  | { readonly ok: true }
  | ({ readonly ok: false } & Omit<AccessRefusal, "ok">)
  /**
   * The request never got an answer. Deliberately distinct from a refusal: an
   * offline tab has not been rejected by anything, and telling the user "that
   * code did not work" when nobody looked at it would be a lie of exactly the
   * kind Q7 forbids.
   */
  | { readonly ok: false; readonly unreachable: true; readonly detail: string };

export type FetchLike = (
  input: string,
  init?: RequestInit,
) => Promise<Response>;

export interface AuthClientOptions {
  readonly fetch?: FetchLike;
  /**
   * The coarse self-description the desktop's browser list shows (2a: `Safari/
   * iOS`). Untrusted free text on the host side, which is why it is stored and
   * displayed but never parsed.
   */
  readonly label?: string;
}

/** `GET /auth/session` — does this browser already hold a working cookie? */
export async function checkSession(
  options: AuthClientOptions = {},
): Promise<AccessResult> {
  const doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);
  let response: Response;
  try {
    response = await doFetch(AUTH_SESSION_PATH, {
      method: "GET",
      credentials: "same-origin",
      headers: { accept: "application/json" },
    });
  } catch (error) {
    return unreachable(error);
  }
  return readResult(response);
}

/**
 * `POST /auth/exchange` — spend a bootstrap code for the cookie.
 *
 * The code travels in the **body**, never a query string or a path, so it
 * cannot land in an access log or a `Referer` even though the fragment kept it
 * out of the request line on the way in.
 */
export async function exchangeCode(
  code: string,
  options: AuthClientOptions = {},
): Promise<AccessResult> {
  const doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);
  const body: Record<string, string> = { code };
  if (options.label !== undefined) {
    body.label = options.label;
  }
  let response: Response;
  try {
    response = await doFetch(AUTH_EXCHANGE_PATH, {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
  } catch (error) {
    return unreachable(error);
  }
  return readResult(response);
}

/**
 * Read either shape of answer.
 *
 * `GET /auth/session` answers `{ authenticated: true }` and
 * `POST /auth/exchange` answers `{ ok: true }`, so both keys count as success —
 * and the HTTP status is checked as well, because a body that says yes with a
 * `401` is not a yes.
 */
async function readResult(response: Response): Promise<AccessResult> {
  let payload: unknown = null;
  try {
    payload = await response.json();
  } catch {
    /** A non-JSON body from a proxy or an error page. `payload` stays null and
     * the status decides. */
  }
  const record: Record<string, unknown> =
    typeof payload === "object" && payload !== null
      ? (payload as Record<string, unknown>)
      : {};

  if (response.ok && (record.ok === true || record.authenticated === true)) {
    return { ok: true };
  }
  return {
    ok: false,
    screen: parseAccessScreen(record.screen),
    reason: typeof record.reason === "string" ? record.reason : "unknown",
    attemptsRemaining: numberOrNull(record.attempts_remaining),
    lockoutSeconds: millisToSeconds(numberOrNull(record.retry_after_ms)),
  };
}

function numberOrNull(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/** Whole seconds, **rounded up** — the same rounding the server applies to its
 * `Retry-After` header, so the countdown never says 0 while the limiter is
 * still refusing. */
function millisToSeconds(ms: number | null): number | null {
  return ms === null ? null : Math.max(1, Math.ceil(ms / 1000));
}

function unreachable(error: unknown): AccessResult {
  return {
    ok: false,
    unreachable: true,
    detail: error instanceof Error ? error.message : String(error),
  };
}
