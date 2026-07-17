#!/usr/bin/env bash
#
# web/deploy/bind-custom-domain.sh
#
# Bind www.flightdeckai.app to the FlightDeck web Container App and issue an
# ACA-managed TLS certificate via CNAME validation.
#
# www is the PRIMARY host. A subdomain validates over CNAME (pure DNS), which
# works behind the ingress IP allowlist and auto-renews — exactly like the
# relay's relay.flightdeckai.app. The apex flightdeckai.app is NOT bound here:
# its managed cert would need an HTTP-01 challenge that the allowlist blocks, so
# the apex -> www redirect is configured at the TransIP registrar instead (see
# web/deploy/README.md).
#
# PREREQUISITE — DNS must already resolve on flightdeckai.app:
#   CNAME  www          -> <APP_FQDN>
#   TXT    asuid.www    -> <customDomainVerificationId>
# (get the values from `setup.sh`'s output, or the `az ... show` queries below).
#
# Usage:  web/deploy/bind-custom-domain.sh
# Requires: az CLI logged in to the "New Orange Internal" subscription.
set -euo pipefail

RG="rg-neworange-flightdeck-dev-neu"
APP="ca-neworange-web-dev-neu"
ENV="cae-neworange-flightdeck-dev-neu"
WWW="www.flightdeckai.app"

echo "==> Adding hostname $WWW to $APP"
az containerapp hostname add --hostname "$WWW" -g "$RG" -n "$APP" -o table

echo "==> Binding $WWW + issuing ACA-managed certificate (CNAME validation)"
az containerapp hostname bind --hostname "$WWW" -g "$RG" -n "$APP" \
  --environment "$ENV" --validation-method CNAME -o table

cat <<EOF

==> Done. Verify (from an allowlisted network):
    curl -sSfI https://$WWW/                       # 200
    curl -sSI  https://flightdeckai.app/ | grep -i location   # 301 -> https://$WWW/ (TransIP redirect)
EOF
