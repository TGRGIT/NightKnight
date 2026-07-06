# NightKnight — Private, Self-Hosted CGM Glucose Dashboard

**A modern continuous glucose monitoring (CGM) platform and native iOS app — [Nightscout](https://github.com/nightscout/cgm-remote-monitor)-compatible, written in Rust, and private by design.** Follow your glucose in real time with a beautiful dashboard, Time-in-Range, AGP, GMI and research-grounded analytics — on infrastructure *you* control.

[<img src="https://developer.apple.com/assets/elements/badges/download-on-the-app-store.svg" alt="Download NightKnight — Glucose Tracker on the App Store" height="52">](https://apps.apple.com/us/app/nightknight-glucose-tracker/id6784815820)

![Platform: iOS · Apple Watch · CarPlay · Web](https://img.shields.io/badge/platform-iOS%20·%20watchOS%20·%20CarPlay%20·%20Web-blue)
![Deploy: Cloudflare Workers or Docker](https://img.shields.io/badge/deploy-Cloudflare%20Workers%20or%20Docker-orange)
![Built with Rust](https://img.shields.io/badge/core-Rust-b7410e)
![License: MIT](https://img.shields.io/badge/license-MIT-green)

> [!NOTE]
> **Status: Early development / Alpha.** NightKnight is a personal-health project. It is
> **not** a medical device — do not use it as the sole basis for treatment decisions.
> Background refresh needs APNs silent push configured to be timely (see
> [docs/SILENT-PUSH.md](docs/SILENT-PUSH.md)); until then it falls back to the OS's
> best-effort `BGAppRefreshTask`.

## Why NightKnight

- **🔒 Private by default.** Your glucose history stays yours — talks only to the server you point it at, no third-party analytics, no ads, no account with us. Every row is scoped to its owner; one person can never see another's data.
- **🦀 One Rust core, two runtimes.** Deploy to **Cloudflare Workers + D1** in minutes, or self-host a **container + Postgres**. All domain, storage, auth, and API logic is shared, so behaviour is byte-identical wherever you run it.
- **🔌 Drop-in Nightscout compatibility.** Speaks the Nightscout v1/v3 API, so the uploaders and follower apps you already use (xDrip+, Loop, AndroidAPS, Trio) keep working out of the box — plus a clean modern `v4` API for first-party clients.
- **📊 Analytics that explain themselves.** Not just a number — Glycemia Risk Index, Ambulatory Glucose Profile, episode detection and advanced variability, every metric pinned to the clinical literature.
- **📱 A polished native experience.** SwiftUI iOS app, Apple Watch app + complication, Home/Lock-Screen widgets, and a CarPlay glance — mg/dL and mmol/L both first-class everywhere.

## Demo

https://github.com/TGRGIT/NightKnight/raw/main/marketing/appstore/previews/iphone-6.9-preview.mp4

> The 30-second App Store preview (also available for [iPad](marketing/appstore/previews/ipad-13-preview.mp4)). Static screenshots are below.

## Screenshots

### iOS App

| Dashboard (mg/dL) | Dashboard (mmol/L) | Statistical Analysis |
|---|---|---|
| ![NightKnight iOS glucose dashboard in mg/dL](marketing/appstore/screenshots/iphone-6.9/01-dashboard.png) | ![NightKnight iOS glucose dashboard in mmol/L](marketing/appstore/screenshots/iphone-6.9/02-dashboard-mmol.png) | ![CGM statistical analysis overview — GRI, GMI, Time-in-Range](marketing/appstore/screenshots/iphone-6.9/03-analysis-overview.png) |

| AGP | Episodes & Variability | Settings |
|---|---|---|
| ![Ambulatory Glucose Profile (AGP) with percentile bands](marketing/appstore/screenshots/iphone-6.9/04-analysis-agp.png) | ![Hypo/hyper episode detection and glucose variability](marketing/appstore/screenshots/iphone-6.9/05-analysis-episodes.png) | ![NightKnight settings — private, self-hosted CGM](marketing/appstore/screenshots/iphone-6.9/06-settings.png) |

### Web UI

| Dashboard | Statistical Analysis |
|---|---|
| ![NightKnight web glucose dashboard](marketing/appstore/screenshots/web/01-dashboard.png) | ![NightKnight web CGM analytics](marketing/appstore/screenshots/web/02-analysis.png) |

### Apple Watch

| Series 11 — mg/dL | Series 11 — mmol/L | Ultra 3 — mg/dL | Ultra 3 — mmol/L |
|---|---|---|---|
| ![Apple Watch Series 11 glucose complication, mg/dL](marketing/appstore/screenshots/watch-series11/01-dashboard-mgdl.png) | ![Apple Watch Series 11 glucose, mmol/L](marketing/appstore/screenshots/watch-series11/02-dashboard-mmol.png) | ![Apple Watch Ultra 3 glucose, mg/dL](marketing/appstore/screenshots/watch-ultra3/01-dashboard-mgdl.png) | ![Apple Watch Ultra 3 glucose, mmol/L](marketing/appstore/screenshots/watch-ultra3/02-dashboard-mmol.png) |

> Full-resolution App Store assets (iOS, iPad, Watch) are in [`marketing/appstore/screenshots/`](marketing/appstore/screenshots/).

## Inspired by Nightscout

**NightKnight is inspired by [Nightscout](https://github.com/nightscout/cgm-remote-monitor)** —
the open-source project that pioneered self-hosted CGM monitoring and has helped countless
people with diabetes and their families. NightKnight builds on the ideas and the open API
that Nightscout established; see [Acknowledgements](#acknowledgements).

## Features in depth

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
- **Native iOS, Watch & CarPlay.** SwiftUI app with an Analysis view, App Intents
  widgets, HealthKit read/write, on-device alarms, an Apple Watch app + complication,
  and a glanceable CarPlay Driving Task screen.
- **Use it without a server, too.** The iOS app can run standalone against **Dexcom
  Share**, **LibreLinkUp**, or **your own Nightscout** instance — computing the full
  analytics on-device (via the shared Rust core over FFI), byte-identical to the server.

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
  nightknight-ffi        C-ABI bridge exposing the analytics core to the iOS app
  nightknight-worker     Cloudflare Worker entrypoint (wasm32)
  nightknight-server     container server entrypoint (axum)
web/dist/                the web SPA (no build step — static files)
ios/                     native SwiftUI app, Apple Watch, widgets, CarPlay + Rust FFI
deploy/                  Dockerfile, docker-compose.yml
branding/                NightKnight app icon (hubsystem style)
docs/                    SETUP, ARCHITECTURE, API-COMPAT, TESTING,
                         openapi.yaml, STATISTICAL-ANALYSIS
```

## Quick start

See **[docs/SETUP.md](docs/SETUP.md)** for the full guide, or
**[docs/DEPLOY-WORKER.md](docs/DEPLOY-WORKER.md)** for the Cloudflare Worker
build/deploy/redeploy runbook. The short version:

- **Cloudflare:** `wrangler d1 create nightknight`, paste the id into
  `service/crates/nightknight-worker/wrangler.toml`, set the Access secrets, then
  from `service/crates/nightknight-worker` run `npx --yes wrangler@latest deploy`.
- **Container:** `cd deploy && cp .env.example .env && docker compose up -d`.

## Status

The service (both runtimes), the web app (Dashboard / Analysis / Settings) and the
native iOS app (SwiftUI tabs + Analysis view + App Intents widgets + HealthKit +
on-device alarms + Apple Watch + CarPlay) are built and tested. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Roadmap / follow-ups

Planned work, not yet implemented:

- **Historical data import.** **LibreView CSV** upload (`POST /api/v4/import/libreview`,
  with a file picker in Settings → Import history) and a **Nightscout source connector**
  (mirror another Nightscout/NightKnight instance by URL + api-secret) now ship —
  normalising into the canonical store with content dedup. Still to come: **Dexcom
  Clarity CSV** and a Nightscout `mongodump`/`treatments` path.
- **Exportable reports.** The deeper analytics (GRI, AGP percentile bands, time-of-day
  patterns, hypo/hyper event detection, advanced variability) now ship in the web and
  iOS *Statistical Analysis* views — see
  [docs/STATISTICAL-ANALYSIS.md](docs/STATISTICAL-ANALYSIS.md) and
  [docs/CGM-ANALYTICS-RESEARCH.md](docs/CGM-ANALYTICS-RESEARCH.md). Still to come: a
  printable AGP one-pager and CSV/JSON export of the computed metric set.
- **Android client.** A native Android app for feature parity with iOS.
- **Sharing / followers.** A scoped, revocable read-only share model so a parent or care
  team member can follow your data without your credentials.
- **OpenAPI specification.** An OpenAPI 3.1 document for the v1/v3/v4 API is published at
  [docs/openapi.yaml](docs/openapi.yaml) (validates clean under Redocly), enabling
  generated clients, contract tests, and interactive docs. Auto-generating it from the
  Rust handlers (rather than hand-maintaining) is a possible follow-up.

## Acknowledgements

NightKnight is inspired by and indebted to **[Nightscout](https://github.com/nightscout/cgm-remote-monitor)**
(the CGM Remote Monitor project) and the wider **#WeAreNotWaiting** community.
Nightscout pioneered open, self-hosted continuous glucose monitoring and built the API
and ecosystem that this project gratefully builds on. Heartfelt thanks to its
maintainers and the many volunteers whose years of open-source work have helped so many
people with diabetes and the people who care for them.

## License

[MIT](LICENSE) © 2026 Fergus Cooney
