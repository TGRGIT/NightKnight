# NightKnight

A secure, modern reimplementation of [Nightscout](https://github.com/nightscout/cgm-remote-monitor)
for continuous glucose monitoring — a Rust service that runs on **Cloudflare Workers
+ D1** *or* as a **self-hosted container + Postgres**, with a matching native iOS app
(in progress). It keeps the original Nightscout API for ecosystem compatibility while
hardening it and adding a clean modern API.

> NightKnight is a personal-health project. It is **not** a medical device. Do not
> use it as the sole basis for treatment decisions.

## What it is

- **One Rust core, two runtimes.** All domain, storage, auth, and API logic lives in
  runtime-agnostic crates shared by the Cloudflare Worker and the container server,
  so behaviour is identical wherever you deploy.
- **Nightscout-compatible.** Implements the v1 and v3 APIs that uploaders (xDrip+,
  Loop, AndroidAPS, Trio) and follower apps already speak, plus a modern `v4` API for
  first-party clients.
- **mg/dL and mmol/L are both first-class.** Every reading remembers the unit it was
  entered in and carries a canonical mg/dL value; the two units mix freely in one
  stream and convert with a single, property-tested constant (18.0156).
- **Multi-user and private by default.** Every row is scoped to its owner; one
  person can never see another's data.
- **Secure by design.** Runs behind Cloudflare Access (or your own reverse proxy);
  credentials are accepted in **headers only**, never the URL query string; device
  tokens are stored only as hashes.
- **A pretty, useful dashboard.** A bespoke glucose chart (target band, threshold
  lines, colour-by-range points) plus Time-in-Range, GMI, estimated A1c and
  variability.
- **Deep, research-grounded analytics.** A dedicated *Statistical Analysis* view (web
  + iOS) with the Glycemia Risk Index, an Ambulatory Glucose Profile, hypo/hyper
  episode detection, time-of-day patterns and advanced variability (SD, J-index, MAGE,
  CONGA, MODD) — every metric explained inline and pinned to the literature
  ([docs/CGM-ANALYTICS-RESEARCH.md](docs/CGM-ANALYTICS-RESEARCH.md)), gap-tolerant, and
  unit-independent.

## Repository layout

```
service/crates/
  nightknight-core       domain model: units, entries/treatments, trend, analytics, time
  nightknight-storage    Storage trait + portable SQL shared by all backends
  nightknight-store-sql  sqlx backend (SQLite for tests + Postgres for the container)
  nightknight-store-d1   Cloudflare D1 backend (wasm32)
  nightknight-auth       Cloudflare Access / OIDC JWT verification + scope model
  nightknight-api        v1 / v3 / v4 HTTP API (transport-agnostic, shared)
  nightknight-connectors Dexcom Share + LibreLinkUp cloud connectors (pure + tested)
  nightknight-worker     Cloudflare Worker entrypoint (wasm32)
  nightknight-server     container server entrypoint (axum)
web/dist/                the web SPA (no build step — static files)
deploy/                  Dockerfile, docker-compose.yml
branding/                NightKnight app icon (hubsystem style)
docs/                    SETUP, ARCHITECTURE, API-COMPAT, TESTING
```

## Quick start

See **[docs/SETUP.md](docs/SETUP.md)** for the full guide. The short version:

- **Cloudflare:** `wrangler d1 create nightknight`, paste the id into
  `service/crates/nightknight-worker/wrangler.toml`, set the Access secrets, then
  `wrangler deploy`.
- **Container:** `cd deploy && cp .env.example .env && docker compose up -d`.

## Status

The service (both runtimes), the web app (Dashboard / Analysis / Settings) and the
native iOS app (SwiftUI tabs + Analysis view + App Intents widgets + HealthKit +
on-device alarms + Apple Watch) are built and tested. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Roadmap / follow-ups

Planned work, not yet implemented:

- **Historical data import.** **LibreView CSV** upload (`POST /api/v4/import/libreview`,
  with a file picker in Settings → Import history) and a **Nightscout source connector**
  (mirror another Nightscout/NightKnight instance by URL + api-secret) now ship —
  normalising into the canonical store with content dedup. Still to come: **Dexcom
  Clarity CSV** and a Nightscout `mongodump`/`treatments` path.
- **CarPlay support.** A CarPlay scene for the iOS app: current glucose + trend and a
  glanceable recent-history view, safe for in-vehicle use.
- **Exportable reports.** The deeper analytics (GRI, AGP percentile bands, time-of-day
  patterns, hypo/hyper event detection, advanced variability) now ship in the web and
  iOS *Statistical Analysis* views — see
  [docs/STATISTICAL-ANALYSIS.md](docs/STATISTICAL-ANALYSIS.md) and
  [docs/CGM-ANALYTICS-RESEARCH.md](docs/CGM-ANALYTICS-RESEARCH.md). Still to come: a
  printable AGP one-pager and CSV/JSON export of the computed metric set.
- **OpenAPI specification.** A published OpenAPI (Swagger) document for the v1/v3/v4
  API, enabling generated clients, contract tests, and interactive docs.
