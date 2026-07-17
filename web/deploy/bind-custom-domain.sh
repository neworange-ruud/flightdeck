#!/usr/bin/env bash
#
# web/deploy/bind-custom-domain.sh
#
# Bind flightdeckai.app (apex) and www.flightdeckai.app to the FlightDeck web
# Container App and issue ACA-managed TLS certificates.
#
# www redirects to the apex at the application layer (see web/next.config.ts),
# so both hostnames are bound to the SAME app.
#
# PREREQUISITE — DNS must already resolve on flightdeckai.app. An apex domain
# cannot be a CNAME, so it uses an A record to the environment's static IP plus
# a TXT for ownership; www uses a CNAME. Get the concrete values from
# `setup.sh`'s final output (or the `az ... show` queries at the bottom):
#
#   A      @            -> <ENV_STATIC_IP>
#   TXT    asuid        -> <customDomainVerificationId>
#   CNAME  www          -> <APP_FQDN>
#   TXT    asuid.www    -> <customDomainVerificationId>
#
# The ingress is IP-restricted (deny-by-default allowlist). We validate cert
# issuance via TXT (apex) and CNAME (www), NOT HTTP, so the ACME challenge never
# needs to reach the IP-blocked ingress — the allowlist stays in place.
# If a future run reports an HTTP/ACME validation failure anyway, temporarily
# remove the IP restrictions, bind, then re-add them.
#
# Usage:  web/deploy/bind-custom-domain.sh
# Requires: az CLI logged in to the "New Orange Internal" subscription.
set -euo pipefail

RG="rg-neworange-flightdeck-dev-neu"
APP="ca-neworange-web-dev-neu"
ENV="cae-neworange-flightdeck-dev-neu"
APEX="flightdeckai.app"
WWW="www.flightdeckai.app"

bind() {
  local host="$1" method="$2"
  echo "==> Adding hostname $host to $APP"
  az containerapp hostname add --hostname "$host" -g "$RG" -n "$APP" -o table
  echo "==> Binding $host + issuing ACA-managed certificate ($method validation)"
  az containerapp hostname bind --hostname "$host" -g "$RG" -n "$APP" \
    --environment "$ENV" --validation-method "$method" -o table
}

# Apex has no CNAME -> validate via the asuid TXT record.
bind "$APEX" TXT
# www resolves via CNAME -> validate via CNAME.
bind "$WWW" CNAME

cat <<EOF

==> Done. Verify (from an allowlisted network):
    curl -sSfI https://$APEX/            # 200
    curl -sSI  https://$WWW/ | grep -i location   # 301 -> https://$APEX/
EOF
