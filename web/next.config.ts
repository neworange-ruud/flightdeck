import type { NextConfig } from "next";

// The canonical host the site is served on. www redirects here (see below).
const CANONICAL_HOST = "flightdeckai.app";

const nextConfig: NextConfig = {
  transpilePackages: ["@flightdeck/docs-platform"],

  // Emit a self-contained server bundle (.next/standalone/server.js) so the
  // container image doesn't need the full node_modules tree — see web/Dockerfile.
  output: "standalone",

  // www.flightdeckai.app -> flightdeckai.app (301). Both hostnames are bound to
  // this Container App; this rule collapses them to the canonical apex so the
  // relay/desktop and search-engine canonicalisation see one origin. Runs in
  // the Next server (standalone), so no separate proxy layer is needed.
  async redirects() {
    return [
      {
        source: "/:path*",
        has: [{ type: "host", value: `www.${CANONICAL_HOST}` }],
        destination: `https://${CANONICAL_HOST}/:path*`,
        permanent: true,
      },
    ];
  },
};

export default nextConfig;
