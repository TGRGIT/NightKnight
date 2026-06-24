# Architecture

NightKnight is one Rust core with two runtimes. Everything that defines behaviour —
the domain model, storage, auth, and the HTTP API — lives in runtime-agnostic crates
shared by both the Cloudflare Worker and the container server. Only the thin
entrypoints differ.

## Crate graph

```
                    nightknight-core   (units, documents, trend, analytics, time)
                          ▲
        ┌─────────────────┼───────────────────┐
nightknight-storage   nightknight-auth         │
   (trait + SQL)      (JWT verify + scopes)     │
        ▲                                        │
   ┌────┴─────┐                                  │
store-sql   store-d1                  nightknight-api  (v1 / v3 / v4, transport-agnostic)
 (sqlx)    (D1/wasm)                          ▲
        ▲     ▲                          ┌─────┴──────┐
        │     └──────────────────────  worker        server
   server (axum)                       (wasm)        (axum)
```

## Key decisions

- **Transport-agnostic API.** `nightknight-api` works on a tiny `ApiRequest` /
  `ApiResponse` model, not a web framework, so the same handlers run under Workers
  and axum and are unit-testable with no server.
- **`Send` across runtimes.** The Workers runtime is single-threaded with `!Send`
  futures; axum needs `Send`. The `Storage` trait requires `Send` everywhere except
  `wasm32` (via `cfg_attr` on `async_trait`), and each backend (compiled for one
  target) picks up the right variant.
- **Portable SQL, two backends.** `nightknight-storage::sql` builds every statement
  with `?` placeholders (D1 and sqlx both use them) and `INSERT … ON CONFLICT`. The
  D1 and sqlx backends execute the identical strings, so the storage contract tests
  (run on SQLite) are strong evidence both agree. The container uses Postgres via the
  same sqlx `Any` code path.
- **Unit-first glucose model.** A `GlucoseValue` stores a canonical mg/dL plus the
  unit it was entered in. All maths is on mg/dL; display honours the requested unit.
  mg/dL and mmol/L mix freely in one stream. The 18.0156 constant is centralised and
  property-tested.
- **Layered auth.** The edge (Cloudflare Access or your proxy) establishes *who the
  human is*; `nightknight-auth` verifies the Access JWT (RS256/JWKS/aud/exp) or a
  proxy header. On top, per-device tokens carry Nightscout v3 scopes
  (`{api}:{collection}:{action}`). Credentials are header-only.

## Connectors

`nightknight-connectors` pulls glucose from vendor clouds — **Dexcom Share** and
**LibreLinkUp**. The protocol (URLs, JSON bodies, response parsing, timestamp/trend
decoding) is pure and unit-tested; I/O goes through an injected `HttpClient` trait, so
the same connector runs on the container (a `reqwest` client) and the Worker
(`worker::Fetch`). The container runs a tokio poll loop
(`nightknight-server/src/connector.rs`) on an interval, ingesting readings via
`ApiService::ingest_entries` (validated + deduplicated). Configuration is via env for
now (a single account); per-user encrypted credentials + the Worker Cron trigger are
the next step. These are unofficial vendor endpoints — best-effort and feature-flagged.

## Realtime (planned)

Data is served by polling today (the dashboard refreshes each minute, matching CGM
cadence). The plan adds a `RealtimeHub` — a Durable Object WebSocket on Workers and a
native WebSocket on the container — with graceful polling fallback. Nothing in the
core depends on it.

## iOS app (Phase 2)

A from-scratch SwiftUI client: current value + trend, the chart at parity with the
web, HealthKit read/write, App-Intent widgets + Lock Screen, multi-day stats, and
**disableable** on-device alarms (out-of-range + rate-of-change, with snooze). It
talks to the `v4` API through the same Access gate.
