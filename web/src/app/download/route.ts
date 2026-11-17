import { NextResponse } from "next/server";

import { latestDesktopReleaseUrl } from "../../lib/latest-release";

// /download: a stable link to the newest FlightDeck Desktop release's
// downloads. Resolved per request (GitHub's answer is cached for an hour).
export const dynamic = "force-dynamic";

export async function GET() {
  return NextResponse.redirect(await latestDesktopReleaseUrl(), 307);
}
