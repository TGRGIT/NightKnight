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

The service (both runtimes) and the web dashboard are built and tested. The native
iOS app (SwiftUI + App Intents widgets + HealthKit + on-device alarms) is the next
phase. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Roadmap / follow-ups

Planned work, not yet implemented:

- **Historical file imports (LibreView / Dexcom / Nightscout).** The live connectors
  only reach back as far as each vendor's follower API allows (Dexcom Share ≤24h,
  LibreLinkUp ≈12h). Add an upload path that ingests full history from account
  exports — LibreView CSV, Dexcom Clarity CSV, and Nightscout `entries`/`treatments`
  JSON (or a `mongodump`) — normalising into the canonical store with dedup.
- **CarPlay support.** A CarPlay scene for the iOS app: current glucose + trend and a
  glanceable recent-history view, safe for in-vehicle use.
- **Statistical analysis.** Deeper analytics beyond TIR/GMI/eA1c — e.g. AGP-style
  percentile bands, day-of-week and time-of-day patterns, hypo/hyper event detection,
  and exportable reports.
- **OpenAPI specification.** A published OpenAPI (Swagger) document for the v1/v3/v4
  API, enabling generated clients, contract tests, and interactive docs.
