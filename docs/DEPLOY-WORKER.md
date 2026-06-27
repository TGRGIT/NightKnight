# Deploying the Cloudflare Worker

Operational runbook for the **`nightknight-worker`** — the wasm Worker that serves the
whole API (`/api/*`) and the static web SPA, backed by Cloudflare **D1**, behind
**Cloudflare Access**. For the high-level "which target do I pick" overview and the
container (Postgres) path, see [SETUP.md](SETUP.md).

> TL;DR for a routine redeploy (everything already set up):
> ```bash
> cd service/crates/nightknight-worker
> npx --yes wrangler@latest deploy
> ```

---

## What gets deployed

`service/crates/nightknight-worker/wrangler.toml` drives everything:

- **Worker code** — `worker-build --release` compiles the Rust workspace to wasm
  (`[build] command`); `wrangler deploy` runs it automatically.
- **Static assets** — the SPA in `web/dist` is served by Cloudflare Static Assets
  (`[assets]`, SPA fallback). The Worker runs first only for `/api/*`
  (`run_worker_first`).
- **D1 database** — bound as `env.DB` (`[[d1_databases]]`). The schema migration runs
  itself on the first request after deploy (idempotent `CREATE TABLE IF NOT EXISTS`).
- **Cron triggers** — connector sync (`[triggers] crons`): every minute (latest),
  hourly (recent backfill), and daily 00:00 UTC (history walk). Needs
  `CF_CONNECTOR_KEY` (below) or sync is disabled.
- **Observability** — Workers Logs on (`[observability]`), 100% sampling.
- **No public route** — `workers_dev` and `preview_urls` are off, so the only way in is
  the Access-gated custom domain.

---

## Prerequisites

- **Rust** (stable) with the wasm target — `rust-toolchain.toml` pins this; otherwise:
  ```bash
  rustup target add wasm32-unknown-unknown
  ```
- **worker-build**:
  ```bash
  cargo install worker-build      # pulls in wasm-opt on first run
  ```
- **Node** (for `npx`/`wrangler`).
- A **Cloudflare account** with a **D1 database** created and its id in `wrangler.toml`.
- **wrangler auth** — either interactive OAuth or a token:
  ```bash
  npx wrangler login                       # interactive (local)
  # or, non-interactive (CI):
  export CLOUDFLARE_API_TOKEN=<token-with-Workers-Scripts-Edit-+-D1-+-Workers-KV>
  ```

> **Always invoke wrangler as `npx --yes wrangler@latest`.** A globally-installed or
> bundled older wrangler (≤ 3.2x) rejects this repo's `wrangler.toml` —
> you'll see errors like `Unexpected fields … [observability]` or
> `"assets.bucket" is required`. `@latest` supports the `[observability]` and
> `[assets]` syntax used here.

---

## First-time setup

Skip any step already done.

1. **Create the D1 database** and paste the id into `wrangler.toml` (`database_id`):
   ```bash
   npx --yes wrangler@latest d1 create nightknight
   ```

2. **Put the app behind Cloudflare Access.** Create an Access application for your
   hostname (e.g. `nightknight.cooney.be`) admitting **OIDC** (passkey via Pocket ID,
   for humans) and **service tokens** (for machine uploaders). If you use the
   `border-patrol` Terraform, follow the `add-external-app` skill. Note the Access
   **Application Audience (AUD)** tag.

3. **Set the non-secret vars** in `wrangler.toml` `[vars]`:
   - `CF_TEAM_DOMAIN` — your Zero Trust team subdomain (e.g. `cooney`).
   - `CF_REQUIRED_GROUP` — the Pocket ID group a human must be in (defence-in-depth on
     top of the edge policy). Leave empty to disable the in-app group check.
   - `APNS_BUNDLE_ID` / `APNS_DEFAULT_ENV` — silent-push topic + default environment
     (already present with sane defaults; only touch for a non-default bundle id).

4. **Set the secrets** (encrypted; never in the repo):
   ```bash
   cd service/crates/nightknight-worker
   # Access JWT audience — from the Terraform output or the Access app's AUD tag:
   npx --yes wrangler@latest secret put CF_ACCESS_AUD
   # Connector-credential encryption key — 32 bytes, hex or base64. WITHOUT this the
   # connector endpoints return 503 and the cron sync (Dexcom/LibreLinkUp/Nightscout)
   # is disabled:
   npx --yes wrangler@latest secret put CF_CONNECTOR_KEY
   ```
   Generate a key with e.g. `openssl rand -hex 32`.

   **Optional — silent push (APNs)** for timely background refresh. Until all three are
   set, push is simply disabled (device-token registration still works), so this can be
   done later. Full guide + rollout: [SILENT-PUSH.md](SILENT-PUSH.md).
   ```bash
   npx --yes wrangler@latest secret put APNS_KEY_P8     # the full .p8 PEM
   npx --yes wrangler@latest secret put APNS_KEY_ID     # 10-char Key ID
   npx --yes wrangler@latest secret put APNS_TEAM_ID    # 10-char Team ID
   ```

5. **Deploy** (next section).

6. **Add the custom domain** in the dashboard (Workers & Pages → `nightknight` →
   Settings → Domains) so the Access policy applies to it.

7. **Mint your first device token** — sign in to the dashboard (*Devices & tokens*) or
   `POST /api/v4/tokens`, and configure your uploader/app with it (see
   [API-COMPAT.md](API-COMPAT.md)).

---

## Build & deploy

From the Worker crate directory:

```bash
cd service/crates/nightknight-worker

# (optional) build the wasm first, to surface compile errors before the upload:
worker-build --release

# build (again, fast) + upload code, assets, and cron triggers:
npx --yes wrangler@latest deploy
```

A successful deploy prints something like:

```
Uploaded nightknight (4.7 sec)
Deployed nightknight triggers (0.6 sec)
  schedule: * * * * *
  schedule: 0 * * * *
  schedule: 0 0 * * *
Current Version ID: 9ad1e25a-7fd9-4463-9bcb-7f3373c884da
```

It is safe to run repeatedly — that **is** the redeploy. Note the **Version ID**; you
can roll back to a previous version in the dashboard (Workers & Pages → `nightknight`
→ Deployments) if needed.

> Run it from the Worker crate dir (paths in `wrangler.toml` — the assets directory and
> the build output — are relative to that file).

---

## Verify

The site sits behind Access, so an unauthenticated request should get a **302 redirect
to the Access login**, *not* a 5xx:

```bash
curl -s -o /dev/null -w "site=%{http_code}\n"           https://your-host/
curl -s -o /dev/null -w "api=%{http_code}\n"            https://your-host/api/v4/status
# 302 on both = deployed and gated. (`/api/v3/version` is open if you want a 200 check.)
```

Then watch **Workers Logs** (Workers & Pages → `nightknight` → Observability) — every
request logs `METHOD path -> status`, and the cron runs log `connector sync (Nm): N
readings ingested`. Sign in as a user and open the dashboard to confirm end-to-end.

For an authenticated smoke test of the API (passing the Access gate with a service
token + a device token), see the curl block in [SETUP.md](SETUP.md#verifying-a-deployment).

---

## `wrangler.toml` reference

| Key | Purpose |
|---|---|
| `name = "nightknight"` | Worker name (and dashboard page). |
| `main = "build/worker/shim.mjs"` | `worker-build` output entrypoint. |
| `workers_dev = false`, `preview_urls = false` | No bypass routes — Access-gated domain only. |
| `[observability] enabled = true` | Workers Logs (status, exceptions, wall-time, `console_log!`). |
| `[build] command = "worker-build --release"` | Run automatically by `wrangler deploy`. |
| `[triggers] crons` | `* * * * *`, `0 * * * *`, `0 0 * * *` — connector sync cadence. |
| `[[d1_databases]] binding = "DB"` | The D1 database (`database_id` is account-specific). |
| `[assets] directory = "../../../web/dist"` | The SPA, SPA fallback, `run_worker_first = ["/api/*"]`. |
| `[vars] CF_TEAM_DOMAIN` / `CF_REQUIRED_GROUP` | Non-secret config (above). |
| `[vars] APNS_BUNDLE_ID` / `APNS_DEFAULT_ENV` | Silent-push topic + default env (see [SILENT-PUSH.md](SILENT-PUSH.md)). |

**Secrets** (set with `wrangler secret put`, listed with `wrangler secret list`):

| Secret | Required | Purpose |
|---|---|---|
| `CF_ACCESS_AUD` | yes | Access JWT audience the Worker verifies (defence-in-depth on the edge). |
| `CF_CONNECTOR_KEY` | for connectors | 32-byte key sealing connector creds; without it the connector sync + endpoints are disabled. |
| `APNS_KEY_P8` / `APNS_KEY_ID` / `APNS_TEAM_ID` | for silent push | See [SILENT-PUSH.md](SILENT-PUSH.md). |

---

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `Unexpected fields … [observability]` or `"assets.bucket" is required` | wrangler too old — use `npx --yes wrangler@latest deploy`. |
| `worker-build: command not found` | `cargo install worker-build`. |
| `error … wasm32-unknown-unknown` | `rustup target add wasm32-unknown-unknown`. |
| `Authentication error` / `not logged in` | `npx wrangler login`, or export `CLOUDFLARE_API_TOKEN`. |
| `Couldn't find a D1 DB` / binding error | `wrangler d1 create nightknight` and paste the id into `wrangler.toml`. |
| Connector endpoints 503 / cron logs "sync disabled" | `CF_CONNECTOR_KEY` secret not set. |
| Site returns a 5xx (not 302) | Check Workers Logs; a 302 to login is the healthy gated state. |
| Connector sync errors in logs | Inspect the cred's `last_status` (shown in the dashboard's connector list). |

---

## See also

- [SETUP.md](SETUP.md) — deployment overview + the container (Postgres) path.
- [API-COMPAT.md](API-COMPAT.md) — the API surface and uploader auth.
- [SILENT-PUSH.md](SILENT-PUSH.md) — adding APNs silent push (Worker secrets + sender).
