# FlightDeck web (Next.js landing page + docs) — container image.
#
# Build context is `web/` (this app path-depends on the local
# `@flightdeck/docs-platform` package at ./docs-platform via a file: dep).
# Build from the web/ directory:
#
#   cd web && docker build -t flightdeck-web .
#
# or, from the repo root:
#
#   docker build -f web/Dockerfile -t flightdeck-web web/
#
# CI/one-time setup use `az acr build` (cloud build) instead of local Docker;
# see web/deploy/README.md.

# --- Dependencies ---------------------------------------------------------
# Install with the lockfile against a context that already contains the local
# file: dependency (docs-platform), so `npm ci` can resolve it.
FROM node:22-alpine AS deps
WORKDIR /app

# docs-platform is a `file:` dependency in package.json; it must be present
# before `npm ci` resolves the lockfile.
COPY package.json package-lock.json ./
COPY docs-platform/package.json ./docs-platform/package.json
RUN npm ci

# --- Builder --------------------------------------------------------------
FROM node:22-alpine AS builder
WORKDIR /app

COPY --from=deps /app/node_modules ./node_modules
COPY . .

# next/font/google fetches fonts at build time and MDX content is read from
# ./docs — both need the full source tree, which is now present.
# `output: "standalone"` (next.config.ts) emits .next/standalone/server.js.
ENV NEXT_TELEMETRY_DISABLED=1
RUN npm run build

# --- Runtime --------------------------------------------------------------
FROM node:22-alpine AS runtime
WORKDIR /app

ENV NODE_ENV=production
ENV NEXT_TELEMETRY_DISABLED=1
# Azure Container Apps convention; the Next standalone server honours PORT and
# HOSTNAME. HOSTNAME=0.0.0.0 makes it listen on all interfaces inside the pod.
ENV PORT=8080
ENV HOSTNAME=0.0.0.0

# Run as an unprivileged user (node:alpine ships uid/gid 1000 `node`).
# The standalone output is self-contained: its trimmed node_modules, the
# static assets, and the public/ tree are all it needs.
COPY --from=builder --chown=node:node /app/.next/standalone ./
COPY --from=builder --chown=node:node /app/.next/static ./.next/static
COPY --from=builder --chown=node:node /app/public ./public

EXPOSE 8080
USER node
CMD ["node", "server.js"]
