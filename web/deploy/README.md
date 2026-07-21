# Deploying the web app to Azure Container Apps

The FlightDeck web app (Next.js landing page + `/docs`) runs on **Azure
Container Apps**, as a **separate Container App sharing the relay's
environment**. This is the runbook. Concrete secret values (subscription ids,
etc.) live in GitHub Actions **variables/secrets** and the Azure portal, not
here.

## Why a separate app (not the relay's container)

| | Relay | Web |
|---|---|---|
| Runtime | Rust, distroless | Node 22, Next.js standalone |
| State | in-process routing → `maxReplicas: 1` **required** | stateless → may scale out |
| Deploy trigger | release / relay changes | release / web changes |

They share the **resource group, ACR, Container Apps environment, and both
managed identities** — so the only incremental cost is the web app's own
compute. Coupling them would force the web app to inherit `maxReplicas: 1` and
rebuild the relay on every content edit.

## Topology

| Thing | Value |
|---|---|
| Resource group | `rg-neworange-flightdeck-dev-neu` (shared) |
| Container Registry | `crneworangeflightdeckdevneu` — Basic, managed-identity pull (shared) |
| Container Apps env | `cae-neworange-flightdeck-dev-neu`, North Europe (shared) |
| Container App | `ca-neworange-web-dev-neu` — `0.5 vCPU / 1 GiB`, `minReplicas: 1`, `maxReplicas: 3` |
| Image | `…/flightdeck-web:<tag>` |
| Pull identity | `id-…-pull` (AcrPull) — shared |
| Deploy identity | `id-…-deploy` (GitHub-OIDC, AcrPush + Contributor on this app) — shared |

**Ingress is public (no IP allowlist).** It's a public landing page and docs
site, so anyone can reach it. Only the **relay** stays IP-restricted
(deny-by-default allowlist) — the web app deliberately does not inherit it. If
you ever need to lock the web app down, add `Allow` rules with
`az containerapp ingress access-restriction {set,remove,list} -g <rg> -n ca-neworange-web-dev-neu`
(any `Allow` rule flips the ingress to deny-by-default for every other source).

## Domains

**`www.flightdeckai.app` is the primary host.** The site is served on www; the
apex redirects to it.

Why not the apex? Azure's *free managed certificate* for an **apex** domain
validates via an **HTTP-01 challenge from DigiCert** (it must reach
`http://flightdeckai.app/.well-known/…` on port 80), whereas a **subdomain**
validates over **CNAME** — pure DNS. The web app's ingress is now public so an
apex HTTP-01 challenge would succeed, but we keep serving on `www` (the existing,
working arrangement) and redirect the apex to it. The relay stays IP-restricted,
which is exactly why `relay.flightdeckai.app` uses CNAME validation.

- **`www.flightdeckai.app`** → bound to the app; ACA-managed cert via **CNAME
  validation**. This is the canonical host.
- **`flightdeckai.app`** (apex) → **301 → www**, done at the **TransIP
  registrar** (registrar URL-forwarding, which brings its own TLS on the apex).
  Not an ACA binding. `next.config.ts` also has a defence-in-depth apex→www
  redirect for the case where the apex ever hits the ingress directly — it must
  never redirect www (that would loop against the registrar redirect).

### DNS + registrar config on `flightdeckai.app`

`setup.sh` prints the exact values at the end. The `relay` records stay as they
are. In the **TransIP DNS panel**, add:

```
CNAME  www          -> <APP_FQDN>                 # az containerapp show … --query properties.configuration.ingress.fqdn
TXT    asuid.www    -> <customDomainVerificationId>
```

`customDomainVerificationId`:
`az containerapp show -g <rg> -n ca-neworange-web-dev-neu --query properties.customDomainVerificationId -o tsv`.

In the **TransIP control panel** (redirect/forwarding, *not* the DNS tab), set:

```
flightdeckai.app  ->  https://www.flightdeckai.app   (301, keep path)
```

Let TransIP manage the apex A record for the redirect — do **not** add a manual
apex A record pointing at the ACA static IP.

## Deploying

**First time / from a laptop:**

```bash
web/deploy/setup.sh            # cloud-build, create app, wire GitHub
# create the DNS records it prints, wait for propagation, then:
web/deploy/bind-custom-domain.sh
```

**Ongoing:** `.github/workflows/web-deploy.yml` runs on **GitHub Release
published** (or manual dispatch): `az acr build` → `az containerapp update` →
verify the ingress responds. Same OIDC identity and `production` environment as
the relay deploy — no new secret.

### GitHub configuration

Reuses the relay's repo **secrets** (`AZURE_CLIENT_ID`, `AZURE_TENANT_ID`,
`AZURE_SUBSCRIPTION_ID`) and **variables** (`AZURE_RESOURCE_GROUP`,
`AZURE_ACR_NAME`). Adds one variable: **`AZURE_WEB_CONTAINERAPP_NAME`**
(`setup.sh` sets it).

## Health

The Next server answers `GET /` (and `/docs`). CI verifies the ingress responds
with a `2xx`/`3xx` (it also tolerates `403`, which would only appear if the app
were re-locked behind an IP allowlist).

## Cost (rough)

Adds ~$15–25/mo for the web app's `0.5 vCPU / 1 GiB` single warm replica (it
scales to 3 only under load). RG/ACR/env/identities are already paid for by the
relay.
