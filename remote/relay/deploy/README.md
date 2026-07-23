# Deploying the relay to Azure Container Apps

The FlightDeck Remote relay runs on **Azure Container Apps**. This document is
the runbook: the shape of the resources and how deploys happen. Concrete values
(subscription/tenant ids, the live app URL, resource names) are **not** committed
here — this is a public repo. They live in the repo's Actions **variables** and
**secrets** (below) and in the Azure portal.

## Topology

| Thing | Notes |
|---|---|
| Resource group | one RG holds everything |
| Container Registry | ACR **Basic**, managed-identity pull (no registry password) |
| Container Apps env | Consumption profile. **Prefer North Europe** — West Europe has repeatedly returned `ManagedEnvironmentCapacityHeavyUsageError` for new environments. |
| Container App | `0.25 vCPU / 0.5 GiB`, `minReplicas: 1`, `maxReplicas: 1` |
| Pull identity | user-assigned MI with **AcrPull** on the registry |
| Deploy identity | user-assigned MI, GitHub-OIDC federated, **AcrPush** + **Container Registry Tasks Contributor** on the registry (the latter is required for web-deploy's `az acr build` step) + **Contributor** on the app only |

**`maxReplicas: 1` is required, not a tuning choice** — routing state is in
process memory (`store::InMemoryStore`), so both legs of a pairing must land on
the same replica. Scaling out needs a shared `RelayStore` (Redis / Azure Table)
first. See `../src/store.rs`.

The Container Apps ingress serves a managed TLS cert on both the
`*.azurecontainerapps.io` hostname (retrieve with
`az containerapp show … --query properties.configuration.ingress.fqdn`) and the
**pinned custom domain `relay.flightdeckai.app`** (remote-control-edn) — the
stable endpoint the desktop + iOS apps use, so a rename/recreate of the Azure
resources no longer orphans pairings. Rebinding the custom domain after such a
move is `bind-custom-domain.sh` (it re-issues the ACA managed cert via CNAME
validation); the two DNS records it needs on `flightdeckai.app` are:

```
CNAME  relay        -> <app>.<env-suffix>.northeurope.azurecontainerapps.io
TXT    asuid.relay  -> <customDomainVerificationId>   # az containerapp show … --query properties.customDomainVerificationId
```

**Ingress is IP-restricted (deny-by-default allowlist)** — only a fixed set of
source IPs may reach the relay. Manage the list with
`az containerapp ingress access-restriction {set,remove,list} -g <rg> -n <app>`.
All rules must share one action (`Allow`); adding any `Allow` rule denies every
other source. Note this means clients (including phones) can only connect from
an allowlisted network.

## Health

- `GET /healthz` → `ok` (liveness) · `GET /readyz` → `ok` (readiness)
- `GET /version` → `{"version":"…","git_sha":"…"}` — `git_sha` is the deployed revision.

## How deploys happen

`.github/workflows/relay-deploy.yml` runs when a **GitHub Release is published**
(or via manual `workflow_dispatch`): it builds the image, pushes it to ACR, runs
`az containerapp update`, then verifies `/version` reports the new SHA. CI
(fmt / clippy / test / docker build-check) is separate, in `relay.yml`, on every
push/PR touching `remote/**`.

Auth is **GitHub OIDC** — no Azure secret is stored in the repo. The deploy job
binds to the `production` GitHub environment; the deploy identity's federated
credential trusts exactly `repo:<owner>/<repo>:environment:production`, and
`azure/login` exchanges the short-lived GitHub token for an Azure token.

> `release` events run the workflow file from the repository's **default
> branch**, so `relay-deploy.yml` must be on `main` for release publishes to
> trigger it.

### GitHub configuration

Repo **variables**: `AZURE_RESOURCE_GROUP`, `AZURE_ACR_NAME`,
`AZURE_CONTAINERAPP_NAME`.
Repo **secrets** (OIDC identifiers, not credentials): `AZURE_CLIENT_ID` (the
deploy identity's client id), `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID`.

## Reproducing the setup

Fill the variables in, then run top to bottom. Idempotent-ish (safe to re-run).

```bash
SUB=<SUBSCRIPTION_ID>
RG=<RESOURCE_GROUP>
ACR=<ACR_NAME>              # globally unique, alphanumeric
ENVNAME=<ENV_NAME>
APP=<APP_NAME>
REPO=<owner>/<repo>
REGION=northeurope         # see the capacity note above

az account set --subscription "$SUB"
az extension add --name containerapp --upgrade

# 1. Resource group + registry.
az group create -n "$RG" -l "$REGION"
az acr create -g "$RG" -n "$ACR" --sku Basic -l "$REGION"

# 2. Pull identity + AcrPull, then cloud-build the image (no local Docker).
az identity create -g "$RG" -n "$APP-pull"
PULL_PRINCIPAL=$(az identity show -g "$RG" -n "$APP-pull" --query principalId -o tsv)
ACR_ID=$(az acr show -n "$ACR" --query id -o tsv)
az role assignment create --assignee-object-id "$PULL_PRINCIPAL" \
  --assignee-principal-type ServicePrincipal --role AcrPull --scope "$ACR_ID"
( cd .. && az acr build --registry "$ACR" \
    --image flightdeck-relay:latest --file relay/Dockerfile . )   # run from remote/

# 3. Container Apps environment.
az containerapp env create -g "$RG" -n "$ENVNAME" -l "$REGION" --logs-destination log-analytics

# 4. The app — pull via the user-assigned identity, single small replica.
PULL_ID_RID=$(az identity show -g "$RG" -n "$APP-pull" --query id -o tsv)
az containerapp create -g "$RG" -n "$APP" --environment "$ENVNAME" \
  --image "$ACR.azurecr.io/flightdeck-relay:latest" \
  --user-assigned "$PULL_ID_RID" \
  --registry-server "$ACR.azurecr.io" --registry-identity "$PULL_ID_RID" \
  --target-port 8080 --ingress external --transport auto \
  --min-replicas 1 --max-replicas 1 --cpu 0.25 --memory 0.5Gi \
  --env-vars PORT=8080 LOG_FORMAT=json GIT_SHA=latest

# 5. GitHub OIDC deploy identity: federated credential for the production
#    environment, AcrPush + Container Registry Tasks Contributor on the
#    registry, Contributor on the app only.
az identity create -g "$RG" -n "$APP-gha"
az identity federated-credential create --identity-name "$APP-gha" -g "$RG" \
  --name gha-production \
  --issuer https://token.actions.githubusercontent.com \
  --subject "repo:${REPO}:environment:production" \
  --audiences api://AzureADTokenExchange
GHA_PRINCIPAL=$(az identity show -g "$RG" -n "$APP-gha" --query principalId -o tsv)
APP_ID=$(az containerapp show -g "$RG" -n "$APP" --query id -o tsv)
az role assignment create --assignee-object-id "$GHA_PRINCIPAL" \
  --assignee-principal-type ServicePrincipal --role AcrPush --scope "$ACR_ID"
# web-deploy's `az acr build` step needs this role too — AcrPush alone isn't
# enough to run ACR Tasks builds.
az role assignment create --assignee-object-id "$GHA_PRINCIPAL" \
  --assignee-principal-type ServicePrincipal --role "Container Registry Tasks Contributor" --scope "$ACR_ID"
az role assignment create --assignee-object-id "$GHA_PRINCIPAL" \
  --assignee-principal-type ServicePrincipal --role Contributor --scope "$APP_ID"

# 6. Wire GitHub (gh CLI, repo admin).
GHA_CLIENT_ID=$(az identity show -g "$RG" -n "$APP-gha" --query clientId -o tsv)
gh api -X PUT "repos/${REPO}/environments/production" --silent
gh variable set AZURE_RESOURCE_GROUP    --repo "$REPO" --body "$RG"
gh variable set AZURE_ACR_NAME          --repo "$REPO" --body "$ACR"
gh variable set AZURE_CONTAINERAPP_NAME --repo "$REPO" --body "$APP"
gh secret   set AZURE_CLIENT_ID         --repo "$REPO" --body "$GHA_CLIENT_ID"
gh secret   set AZURE_TENANT_ID         --repo "$REPO" --body "$(az account show --query tenantId -o tsv)"
gh secret   set AZURE_SUBSCRIPTION_ID   --repo "$REPO" --body "$SUB"
```

## Cost (rough — verify on the Azure pricing calculator)

- App, always-on 0.25 vCPU / 0.5 GiB, single replica: ~$12–15/mo. A held-open
  WebSocket bills at the active rate, so scale-to-zero savings don't apply —
  `minReplicas: 1` is intentional (avoids reconnect cold-starts).
- ACR Basic: ~$5/mo. · Log Analytics: within the free grant at this volume.

≈ **$17–20/month**. To drop the ACR cost, images could move to GitHub Container
Registry (free) with an ACA registry credential — not done here, to keep the
pull path on managed identity (no PAT to rotate).

## Not addressed yet

- **Persistent `RelayStore`** — removes `maxReplicas: 1` and survives restarts
  without dropping pairings/queues (`../src/store.rs`).
- **Pinned custom domain** (`relay.flightdeck.app`) + wiring the desktop's
  `relay_url` config to it.
