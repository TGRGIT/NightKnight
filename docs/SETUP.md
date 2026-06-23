# Deploying NightKnight

Two co-equal deployment targets share one codebase. Pick whichever suits you.

- **Path A — Cloudflare Workers + D1** (serverless, behind Cloudflare Access).
- **Path B — Container + Postgres** (self-hosted, behind any reverse proxy).

Then point the (forthcoming) iOS app at whichever base URL you deployed.

---

## Prerequisites

- Rust (stable) with the wasm target: `rustup target add wasm32-unknown-unknown`
- For Path A: Node + `npm i -g wrangler` (recent — needs `run_worker_first`),
  and `cargo install worker-build`
- For Path B: Docker + Docker Compose

Build & test the whole service first:

```bash
cargo test --workspace          # 70+ tests
cargo clippy --workspace --all-targets
```

---

## Path A — Cloudflare (Workers + D1)

1. **Create the D1 database** and copy the id into
   `service/crates/nightknight-worker/wrangler.toml` (`database_id`):

   ```bash
   wrangler d1 create nightknight
   ```

2. **Put the app behind Cloudflare Access.** Create an Access application for your
   hostname (e.g. `nightknight.cooney.be`) admitting **OIDC** (passkey via Pocket ID)
   and **service tokens** (for uploaders). If you use the `border-patrol` Terraform,
   follow the `add-external-app` skill: add `terraform/apps/nightknight.tf` calling
   `modules/external_app` with `auth_methods = ["oidc", "service_token"]` (and
   `required_oidc_group` to gate by group), then `make apply`.

3. **Set the Worker config:**
   - In `wrangler.toml`, set `CF_TEAM_DOMAIN` to your Zero Trust team subdomain.
   - Set the Access AUD as a secret:
     ```bash
     # from the terraform output, or the Access app's Application Audience tag
     wrangler secret put CF_ACCESS_AUD
     ```

4. **Deploy** (from the worker crate dir):

   ```bash
   cd service/crates/nightknight-worker
   wrangler deploy
   ```

   The first request runs the schema migration automatically. The SPA in `web/dist`
   is served by Static Assets; the Worker handles `/api/*` only.

5. **Add the custom domain** in the Cloudflare dashboard (Workers & Pages → your
   worker → Settings → Domains) so the Access policy applies.

6. **Create your first device token** from the dashboard (*Devices & tokens*), or via
   the API once signed in. Configure your uploader with it (see API-COMPAT.md).

### How uploaders authenticate behind Access

Machine clients can't do interactive login, so they pass the Access gate with a
**Cloudflare Access service token** (`CF-Access-Client-Id` / `CF-Access-Client-Secret`
headers) and additionally send their **NightKnight device token** (in the `api-secret`
header or `Authorization: Bearer`) to select the user and scope. Browsers use the
passkey/OIDC session cookie automatically.

---

## Path B — Container (Docker + Postgres)

```bash
cd deploy
cp .env.example .env          # set POSTGRES_PASSWORD etc.
docker compose up -d --build
```

This starts Postgres and the server (migrations applied on boot). Front it with your
identity provider / reverse proxy:

- Run something like **oauth2-proxy** (with Pocket ID) or **APISIX** in front.
- It must authenticate the user and forward their email in the header named by
  `NK_AUTH_HEADER` (default `x-auth-request-email`), and **strip that header from
  inbound client requests** so it can't be spoofed.

For purely local testing without a proxy, set `NK_AUTH_MODE=dev` (a single fixed
demo user) — never use `dev` in production.

### Environment variables

| Var | Default | Meaning |
|---|---|---|
| `NK_DATABASE_URL` | `sqlite://nightknight.db?mode=rwc` | `postgres://…` or `sqlite://…` |
| `NK_AUTH_MODE` | `trust-header` | `trust-header` \| `dev` \| `none` |
| `NK_AUTH_HEADER` | `x-auth-request-email` | header carrying the user's email |
| `NK_BIND` | `0.0.0.0:8787` | listen address |
| `NK_WEB_DIR` | `web/dist` | static SPA directory |
| `NK_LOG` | `info` | tracing filter |

> **TLS to a remote Postgres:** the default build connects without TLS (fine inside a
> compose network). For a TLS-required managed Postgres, enable a sqlx TLS feature in
> `nightknight-store-sql` and use an `sslmode=require` URL.

---

## Verifying a deployment

```bash
BASE=https://your-host
# (Behind Access, include the service-token headers on these calls.)
curl "$BASE/api/v3/version"                      # no auth needed
curl -H "api-secret: <device-token>" "$BASE/api/v1/status.json"
curl -H "api-secret: <device-token>" -X POST "$BASE/api/v1/entries" \
  -H 'content-type: application/json' \
  -d '[{"type":"sgv","date":'"$(($(date +%s)*1000))"',"sgv":120,"direction":"Flat"}]'
curl -H "api-secret: <device-token>" "$BASE/api/v1/entries.json?count=5"
```

Then open the dashboard in a browser as a signed-in user.
