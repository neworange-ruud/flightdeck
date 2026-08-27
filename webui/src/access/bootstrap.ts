import { ACCESS_CODE_LENGTH } from "../state/model";

/**
 * Q4 — the bootstrap code arrives in the **URL fragment**, and is stripped from
 * history.
 *
 * ```
 * short code    ~120s bootstrap, shown on the desktop with a countdown
 *                    │  exchanged once, POST /auth/exchange
 *                    ▼
 * HttpOnly cookie    long-lived, per browser, revocable from the desktop
 * ```
 *
 * The fragment is the transport for one specific reason: **a fragment is never
 * sent to the server.** It cannot appear in a request line, an access log, a
 * `Referer` header, or a reverse proxy's history — which a query string or a
 * path segment all can. 2a's QR encodes exactly this:
 * `http://192.168.2.14:7420/#8412`.
 *
 * The second half is this module's actual job: **strip it from history**, so a
 * bookmark, a reload, a shared URL or a browser-restore carries no credential.
 * Q4's promise is "bookmarks then work with no credential visible anywhere",
 * and a fragment that stays in the address bar keeps it visible in the most
 * durable place a browser has.
 *
 * ## Why the strip happens *before* the exchange, not after
 *
 * The code is read and the URL rewritten in one step (`consumeBootstrapCode`),
 * and only then is the network touched. If the strip waited for a successful
 * exchange, then every failure mode — offline, host restarting, code expired,
 * or the user closing the tab mid-request — would leave a live credential in
 * history. Failing *after* the strip costs the user one re-entry on 2b's code
 * screen, which is the screen that exists for exactly that. Losing a credential
 * into history costs them the credential.
 *
 * Injected `Location`/`History` rather than reaching for globals, so this is a
 * plain unit test with no jsdom navigation involved.
 */

/** The two things this module needs from a browser. */
export interface UrlBar {
  /** The full current URL, fragment included. */
  readonly href: string;
  /**
   * Replace the current history entry's URL — `history.replaceState`, never
   * `pushState`. Pushing would leave the credential one Back button away, which
   * is not removing it from history at all.
   */
  replace(url: string): void;
}

/** The real browser, wired at the entry point. */
export function windowUrlBar(): UrlBar {
  return {
    get href() {
      return window.location.href;
    },
    replace(url) {
      window.history.replaceState(null, "", url);
    },
  };
}

/**
 * The code in a URL's fragment, or `null`.
 *
 * Accepts the bare digits 2a's QR encodes (`#8412`) and the explicit
 * `#code=8412` form, and **nothing else**: the fragment is also a legitimate
 * place for an anchor or a router path, so anything that is not exactly
 * `ACCESS_CODE_LENGTH` digits is left alone rather than being half-read as a
 * credential.
 */
export function bootstrapCodeIn(href: string): string | null {
  const hash = fragmentOf(href);
  if (hash === null) {
    return null;
  }
  const raw = hash.startsWith("code=") ? hash.slice("code=".length) : hash;
  const pattern = new RegExp(`^[0-9]{${ACCESS_CODE_LENGTH}}$`);
  return pattern.test(raw) ? raw : null;
}

/** The same URL with no fragment at all, and no trailing `#` left behind. */
export function withoutFragment(href: string): string {
  const index = href.indexOf("#");
  return index === -1 ? href : href.slice(0, index);
}

/**
 * Read the code and remove it from history, in one step.
 *
 * Returns the code for the caller to exchange, or `null` when the URL carried
 * none — in which case history is left exactly as it was, because rewriting a
 * URL that had no credential in it would throw away a legitimate anchor for no
 * benefit.
 *
 * After this returns non-`null`, the credential exists **only** in the returned
 * string: not in the address bar, not in the current history entry, and not in
 * a previous one (`replaceState`, not `pushState`).
 */
export function consumeBootstrapCode(bar: UrlBar): string | null {
  const code = bootstrapCodeIn(bar.href);
  if (code === null) {
    return null;
  }
  bar.replace(withoutFragment(bar.href));
  return code;
}

function fragmentOf(href: string): string | null {
  const index = href.indexOf("#");
  if (index === -1) {
    return null;
  }
  const hash = href.slice(index + 1);
  return hash === "" ? null : hash;
}
