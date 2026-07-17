import type { NextConfig } from "next";

// The canonical host the site is served on. The apex redirects here (see below).
//
// www is primary (not the apex): a subdomain gets a free, auto-renewing ACA
// managed cert via CNAME validation, which works behind the ingress IP
// allowlist. The apex can't (its managed cert needs an HTTP-01 challenge that
// the allowlist blocks), so flightdeckai.app -> www.flightdeckai.app is done at
// the TransIP registrar. See web/deploy/README.md.
const CANONICAL_HOST = "www.flightdeckai.app";
const APEX_HOST = "flightdeckai.app";

const nextConfig: NextConfig = {
  transpilePackages: ["@flightdeck/docs-platform"],

  // Emit a self-contained server bundle (.next/standalone/server.js) so the
  // container image doesn't need the full node_modules tree — see web/Dockerfile.
  output: "standalone",

  // Defence-in-depth apex -> www (301). The registrar already redirects the
  // apex, so the app normally only ever sees Host: www.flightdeckai.app; this
  // rule just guarantees the app never serves content on the bare apex if the
  // apex is ever pointed straight at the ingress. It must NOT redirect www
  // (that would loop against the registrar's apex->www redirect).
  async redirects() {
    return [
      {
        source: "/:path*",
        has: [{ type: "host", value: APEX_HOST }],
        destination: `https://${CANONICAL_HOST}/:path*`,
        permanent: true,
      },
    ];
  },
};

export default nextConfig;
