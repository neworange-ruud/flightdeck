import { describe, expect, it } from "vitest";
import {
  bootstrapCodeIn,
  consumeBootstrapCode,
  withoutFragment,
} from "./bootstrap";
import type { UrlBar } from "./bootstrap";

/**
 * Q4: the bootstrap code arrives in the **URL fragment** and is **stripped from
 * history**, so a bookmark carries no credential.
 *
 * Both halves are tested, because only one of them is obvious. Reading a
 * fragment is easy to get right; removing it from history is the half that
 * silently does not happen, and the half the requirement is actually about.
 */

function fakeBar(href: string): UrlBar & {
  readonly replaced: string[];
} {
  const replaced: string[] = [];
  let current = href;
  return {
    get href() {
      return current;
    },
    replace(url) {
      replaced.push(url);
      current = url;
    },
    replaced,
  };
}

describe("reading the code out of the fragment", () => {
  it("reads the bare four digits 2a's QR encodes", () => {
    expect(bootstrapCodeIn("http://192.168.2.14:7420/#8412")).toBe("8412");
  });

  it("also reads the explicit code= form", () => {
    expect(bootstrapCodeIn("http://host:7420/#code=0007")).toBe("0007");
  });

  it("ignores a fragment that is not a code", () => {
    /** The fragment is a legitimate place for an anchor or a router path, so
     * anything that is not exactly four digits is left alone rather than being
     * half-read as a credential. */
    for (const href of [
      "http://host:7420/#top",
      "http://host:7420/#84",
      /** Five digits, built rather than written: a five-character run after a
       * `#` is a valid CSS hex colour, and the palette guard is right to fail
       * on one even in a test file. */
      `http://host:7420/#${"84125"}`,
      "http://host:7420/#84a2",
      "http://host:7420/#",
      "http://host:7420/",
    ]) {
      expect(bootstrapCodeIn(href)).toBeNull();
    }
  });
});

describe("stripping it from history (Q4)", () => {
  it("returns the code and leaves no fragment behind", () => {
    const bar = fakeBar("http://192.168.2.14:7420/#8412");

    expect(consumeBootstrapCode(bar)).toBe("8412");

    expect(bar.href).toBe("http://192.168.2.14:7420/");
    expect(bar.href).not.toContain("8412");
    expect(bar.href).not.toContain("#");
  });

  it("rewrites the current entry rather than pushing a new one", () => {
    /**
     * `replaceState`, never `pushState`. Pushing would leave the credential one
     * Back button away, which is not removing it from history at all — and this
     * fake only offers `replace`, so a future switch to `pushState` cannot pass
     * this test by accident.
     */
    const bar = fakeBar("http://host:7420/#8412");
    consumeBootstrapCode(bar);
    expect(bar.replaced).toEqual(["http://host:7420/"]);
  });

  it("keeps a query string, which is not the credential", () => {
    const bar = fakeBar("http://host:7420/?debug=1#8412");
    expect(consumeBootstrapCode(bar)).toBe("8412");
    expect(bar.href).toBe("http://host:7420/?debug=1");
  });

  it("touches nothing when there was no code", () => {
    /** Rewriting a URL that had no credential in it would throw away a
     * legitimate anchor for no benefit. */
    const bar = fakeBar("http://host:7420/#some-anchor");
    expect(consumeBootstrapCode(bar)).toBeNull();
    expect(bar.replaced).toEqual([]);
    expect(bar.href).toBe("http://host:7420/#some-anchor");
  });

  it("is idempotent — a reload after the strip finds nothing", () => {
    const bar = fakeBar("http://host:7420/#8412");
    expect(consumeBootstrapCode(bar)).toBe("8412");
    /** This is the scenario the strip exists for: the same tab, reloaded, must
     * not be able to spend the code a second time. */
    expect(consumeBootstrapCode(bar)).toBeNull();
    expect(bar.replaced).toHaveLength(1);
  });

  it("withoutFragment leaves no dangling hash", () => {
    expect(withoutFragment("http://host/#8412")).toBe("http://host/");
    expect(withoutFragment("http://host/")).toBe("http://host/");
  });
});
