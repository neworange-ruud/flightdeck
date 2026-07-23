#!/usr/bin/env bash
#
# remote/relay/deploy/bind-custom-domain.sh
#
# Bind the stable custom domain relay.flightdeckai.app to the FlightDeck Remote
# relay Container App and issue an ACA-managed TLS certificate
# (remote-control-edn).
#
# PREREQUISITE — DNS must already be in place on flightdeckai.app:
#   CNAME  relay        -> ca-neworange-flightdeck-dev-neu.niceground-5e920aa9.northeurope.azurecontainerapps.io
#   TXT    asuid.relay  -> 3E673842545B5CA99DA76EBD6EC64972FADFED4A2E9F4DBD6F25313658E75056
# (the TXT is this app's customDomainVerificationId; verify with
#  `az containerapp show -g <rg> -n <app> --query properties.customDomainVerificationId`.)
#
# The relay ingress is open (access is gated by the shared FLIGHTDECK_RELAY_PASSWORD,
# not by a source-IP allowlist — remote-control-uq7). ACA managed certificate
# issuance validates domain ownership via the asuid TXT + CNAME, which is
# unaffected by ingress access rules either way, so this script needs no special
# handling for it.
#
# Usage:  remote/relay/deploy/bind-custom-domain.sh
# Requires: az CLI logged in to the "New Orange Internal" subscription.

set -euo pipefail

RG="rg-neworange-flightdeck-dev-neu"
APP="ca-neworange-flightdeck-dev-neu"
ENV="cae-neworange-flightdeck-dev-neu"
HOSTNAME="relay.flightdeckai.app"

echo "==> Adding hostname $HOSTNAME to $APP"
az containerapp hostname add \
  --hostname "$HOSTNAME" \
  --resource-group "$RG" \
  --name "$APP" \
  -o table

echo "==> Binding hostname + issuing ACA-managed certificate (CNAME validation)"
az containerapp hostname bind \
  --hostname "$HOSTNAME" \
  --resource-group "$RG" \
  --name "$APP" \
  --environment "$ENV" \
  --validation-method CNAME \
  -o table

echo "==> Done. Verify:"
echo "    curl -sSf https://$HOSTNAME/health   # once the relay exposes a health route"
echo "    wss endpoint: wss://$HOSTNAME/ws"
