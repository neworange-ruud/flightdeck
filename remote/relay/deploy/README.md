# Deploying the relay to Azure Container Apps

This document is a reference for the **one-time setup** of the Azure
resources the relay runs on, plus how `containerapp.yaml` in this directory
gets applied. Nothing here has been run against a real Azure subscription —
these are the commands *to* run, not a record of what's already provisioned.

No deploy happens automatically from a developer machine: `.github/workflows/relay.yml`
runs the deploy job in CI, gated on `main` and on the `AZURE_CREDENTIALS`
(and related) repository secrets being present.

## Prerequisites

- An Azure subscription and the `az` CLI, logged in (`az login`) with rights
  to create resource groups, ACR, and Container Apps resources.
- The `containerapp` extension: `az extension add --name containerapp --upgrade`.

## One-time setup

Replace the placeholders (`<...>`) with real values. These commands are
idempotent-ish (safe to re-run) but are meant to be run **once** per
environment (e.g. once for staging, once for production).

```bash
# 1. Resource group — everything below lives in this.
az group create \
  --name <RESOURCE_GROUP> \
  --location <AZURE_REGION>

# 2. Azure Container Registry — holds the relay's Docker images.
az acr create \
  --resource-group <RESOURCE_GROUP> \
  --name <ACR_NAME> \
  --sku Basic

# 3. Container Apps environment — the shared compute/networking boundary
#    Container Apps run inside. One environment can host multiple apps;
#    reuse an existing one if FlightDeck already has one for this region.
az containerapp env create \
  --resource-group <RESOURCE_GROUP> \
  --name <CONTAINERAPP_ENVIRONMENT_NAME> \
  --location <AZURE_REGION>

# 4. Create the Container App itself, pointed at a placeholder image for
#    now (the real image comes from CI on the first successful build). This
#    also provisions the app's system-assigned managed identity.
az containerapp create \
  --resource-group <RESOURCE_GROUP> \
  --name flightdeck-relay \
  --environment <CONTAINERAPP_ENVIRONMENT_NAME> \
  --image mcr.microsoft.com/k8se/quickstart:latest \
  --target-port 8080 \
  --ingress external \
  --min-replicas 1 \
  --max-replicas 2 \
  --system-assigned

# 5. Grant that managed identity `AcrPull` on the registry, then wire the
#    registry to the app by identity (no stored password/secret).
ACR_ID=$(az acr show --name <ACR_NAME> --query id -o tsv)
IDENTITY_PRINCIPAL_ID=$(az containerapp show \
  --resource-group <RESOURCE_GROUP> \
  --name flightdeck-relay \
  --query identity.principalId -o tsv)

az role assignment create \
  --assignee "$IDENTITY_PRINCIPAL_ID" \
  --role AcrPull \
  --scope "$ACR_ID"

az containerapp registry set \
  --resource-group <RESOURCE_GROUP> \
  --name flightdeck-relay \
  --server <ACR_NAME>.azurecr.io \
  --identity system

# 6. Create a service principal for GitHub Actions to deploy with, scoped to
#    just this resource group, and add its JSON output as the
#    `AZURE_CREDENTIALS` repository secret (Settings → Secrets and variables
#    → Actions). See .github/workflows/relay.yml for the other secrets the
#    deploy job expects (registry name, resource group, app name — passed as
#    plain repo variables/secrets, not sensitive on their own).
az ad sp create-for-rbac \
  --name "flightdeck-relay-deploy" \
  --role contributor \
  --scopes "/subscriptions/<SUBSCRIPTION_ID>/resourceGroups/<RESOURCE_GROUP>" \
  --sdk-auth
```

After this one-time setup, redeploying a new image is what
`.github/workflows/relay.yml`'s `deploy` job does on every push to `main`:
build the image, push it to `<ACR_NAME>.azurecr.io`, then

```bash
az containerapp update \
  --resource-group <RESOURCE_GROUP> \
  --name flightdeck-relay \
  --image <ACR_NAME>.azurecr.io/flightdeck-relay:<TAG>
```

`containerapp.yaml` in this directory documents the full desired
configuration (ingress, probes, scaling, env) as a single reviewable
artifact; `az containerapp update --yaml deploy/containerapp.yaml` applies it
wholesale if configuration (not just the image) has changed.

## What's still a placeholder

- `<RESOURCE_GROUP>`, `<AZURE_REGION>`, `<ACR_NAME>`,
  `<CONTAINERAPP_ENVIRONMENT_NAME>`, `<SUBSCRIPTION_ID>` — pick these when
  actually provisioning; they don't exist yet.
- The `AZURE_CREDENTIALS` secret and its siblings referenced by
  `.github/workflows/relay.yml` — not set on this repository yet, which is
  exactly why that workflow's deploy job is written to skip gracefully when
  they're absent.
- Custom domain / TLS cert for a stable relay hostname the desktop app and
  iOS app can hardcode — not addressed here.
- Everything routing/auth-shaped (connection affinity across replicas,
  pairing-ID based routing) — see `src/router.rs`'s doc comment. `scale.maxReplicas: 2`
  in `containerapp.yaml` is deliberately conservative until that's designed.
