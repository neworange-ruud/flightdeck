#!/usr/bin/env bash
#
# web/deploy/setup.sh
#
# Build and deploy the FlightDeck web app (Next.js landing page + docs) to Azure
# Container Apps, reusing the resource group / ACR / Container Apps environment
# already provisioned for the relay (see remote/relay/deploy/README.md).
#
# The web app is a SEPARATE Container App in the SAME environment as the relay:
# different runtime (Node/Next vs Rust), different scaling profile (stateless,
# so it may scale out), independent deploy cadence.
#
# Idempotent-ish: safe to re-run. It cloud-builds the image (no local Docker),
# creates-or-updates the app, mirrors the relay's IP allowlist onto it, and
# grants the existing GitHub-OIDC deploy identity rights over the new app.
#
# Requires: az CLI logged in to the "New Orange Internal" subscription, and
# (for the final GitHub-wiring step) the gh CLI with repo admin.
#
# Usage:  web/deploy/setup.sh
set -euo pipefail

# ---- Existing shared resources (from the relay setup) --------------------
SUB="440c0544-705f-46a9-9c21-31b14dce445e"     # New Orange Internal
RG="rg-neworange-flightdeck-dev-neu"
ACR="crneworangeflightdeckdevneu"
ENVNAME="cae-neworange-flightdeck-dev-neu"
REGION="northeurope"
PULL_ID="id-neworange-flightdeck-dev-neu-pull"     # AcrPull, reused
DEPLOY_ID="id-neworange-flightdeck-dev-neu-deploy" # GitHub-OIDC, reused
RELAY_APP="ca-neworange-flightdeck-dev-neu"         # source of the IP allowlist

# ---- The new web app ------------------------------------------------------
APP="ca-neworange-web-dev-neu"
IMAGE_REPO="flightdeck-web"
TAG="${1:-latest}"
REPO="neworange-ruud/flightdeck"

IMAGE="$ACR.azurecr.io/$IMAGE_REPO:$TAG"

az account set --subscription "$SUB"
az extension add --name containerapp --upgrade -y >/dev/null

# 1. Cloud-build the image from the web/ context (no local Docker needed).
#    Run from web/ so the Dockerfile's expected context (with docs-platform) is
#    the build root.
echo "==> Building $IMAGE via ACR cloud build"
( cd "$(dirname "$0")/.." && az acr build --registry "$ACR" --image "$IMAGE_REPO:$TAG" --file Dockerfile . )

PULL_ID_RID=$(az identity show -g "$RG" -n "$PULL_ID" --query id -o tsv)

# 2. Create the app if absent, else update its image. Stateless => it may scale
#    out; min 1 keeps the public page warm (no cold start), max 3 gives headroom.
if az containerapp show -g "$RG" -n "$APP" >/dev/null 2>&1; then
  echo "==> Updating existing app $APP to $IMAGE"
  az containerapp update -g "$RG" -n "$APP" --image "$IMAGE" -o none
else
  echo "==> Creating app $APP"
  az containerapp create -g "$RG" -n "$APP" --environment "$ENVNAME" \
    --image "$IMAGE" \
    --user-assigned "$PULL_ID_RID" \
    --registry-server "$ACR.azurecr.io" --registry-identity "$PULL_ID_RID" \
    --target-port 8080 --ingress external --transport auto \
    --min-replicas 1 --max-replicas 3 --cpu 0.5 --memory 1.0Gi \
    --env-vars PORT=8080 NODE_ENV=production HOSTNAME=0.0.0.0 -o none
fi

# 3. Mirror the relay's IP allowlist onto the web app (deny-by-default).
#    All rules share action=Allow; adding any Allow rule denies every other
#    source. We read the live relay rules so the two apps never drift.
echo "==> Applying IP allowlist (copied from $RELAY_APP)"
az containerapp ingress access-restriction list -g "$RG" -n "$RELAY_APP" -o json \
  | python3 -c '
import json,sys
for r in json.load(sys.stdin):
    print(r["name"], r["ipAddressRange"], r.get("description",""), sep="\t")
' | while IFS=$'\t' read -r name range desc; do
    az containerapp ingress access-restriction set -g "$RG" -n "$APP" \
      --rule-name "$name" --ip-address "$range" --action Allow \
      --description "$desc" -o none
    echo "    + $name $range"
  done

# 4. Grant the existing GitHub-OIDC deploy identity Contributor on THIS app so
#    the web-deploy workflow can push revisions (AcrPush on the registry is
#    already held from the relay setup).
echo "==> Granting deploy identity Contributor on $APP"
GHA_PRINCIPAL=$(az identity show -g "$RG" -n "$DEPLOY_ID" --query principalId -o tsv)
APP_ID=$(az containerapp show -g "$RG" -n "$APP" --query id -o tsv)
az role assignment create --assignee-object-id "$GHA_PRINCIPAL" \
  --assignee-principal-type ServicePrincipal --role Contributor --scope "$APP_ID" \
  -o none 2>/dev/null || echo "    (role assignment already present)"

# 5. Wire GitHub so .github/workflows/web-deploy.yml can find the app.
#    RG/ACR vars and the OIDC secrets are shared with the relay workflow.
if command -v gh >/dev/null 2>&1; then
  echo "==> Setting GitHub variable AZURE_WEB_CONTAINERAPP_NAME"
  gh variable set AZURE_WEB_CONTAINERAPP_NAME --repo "$REPO" --body "$APP" || true
fi

FQDN=$(az containerapp show -g "$RG" -n "$APP" --query properties.configuration.ingress.fqdn -o tsv)
VERIFY_ID=$(az containerapp show -g "$RG" -n "$APP" --query properties.customDomainVerificationId -o tsv)
ENV_STATIC_IP=$(az containerapp env show -g "$RG" -n "$ENVNAME" --query properties.staticIp -o tsv)

cat <<EOF

==> Deployed.
    App FQDN         : https://$FQDN
    Domain verify id : $VERIFY_ID         (for the asuid.www TXT record)

www is the PRIMARY host (subdomain -> CNAME validation works behind the IP
allowlist and auto-renews). The apex can't get a managed cert behind the
allowlist, so apex -> www is a TransIP registrar redirect, not an ACA binding.

1. DNS records on flightdeckai.app (TransIP DNS panel):
     CNAME  www          -> $FQDN
     TXT    asuid.www    -> $VERIFY_ID

2. TransIP registrar redirect (TransIP control panel, not the DNS tab):
     Redirect  flightdeckai.app  ->  https://www.flightdeckai.app  (301, "with www"/keep-path)
   This also manages the apex A record for you — do NOT add a manual apex A
   record pointing at the ACA static IP ($ENV_STATIC_IP).

3. Once the www records propagate, issue the cert:
     web/deploy/bind-custom-domain.sh
EOF
