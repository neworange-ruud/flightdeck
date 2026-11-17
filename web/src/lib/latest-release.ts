// The newest FlightDeck Desktop release on GitHub, for the site's /download
// link. Desktop releases are tagged desktop-v*; GitHub's own "latest" release
// is the command-line tool's (v*), so /releases/latest would be the wrong page.
const REPO = "neworange-ruud/flightdeck";

/** Where to send people when GitHub can't be asked: every Desktop release. */
export const DESKTOP_RELEASES_FALLBACK = `https://github.com/${REPO}/releases?q=desktop&expanded=true`;

type Release = { tag_name: string; html_url: string; draft: boolean; prerelease: boolean };

/**
 * The release page (with every platform's download) of the newest published
 * Desktop release. GitHub's answer is cached for an hour, which keeps well
 * inside the API's unauthenticated rate limit.
 */
export async function latestDesktopReleaseUrl(): Promise<string> {
  try {
    const res = await fetch(`https://api.github.com/repos/${REPO}/releases?per_page=30`, {
      headers: { Accept: "application/vnd.github+json", "User-Agent": "flightdeck-web" },
      next: { revalidate: 3600 },
    });
    if (!res.ok) return DESKTOP_RELEASES_FALLBACK;
    const releases = (await res.json()) as Release[];
    const latest = releases.find(
      (r) => r.tag_name.startsWith("desktop-v") && !r.draft && !r.prerelease,
    );
    return latest?.html_url ?? DESKTOP_RELEASES_FALLBACK;
  } catch {
    return DESKTOP_RELEASES_FALLBACK;
  }
}
