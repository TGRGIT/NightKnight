# Testing

NightKnight is a health application, so the tests are written to be **read by a
human** as much as run by a machine: each one states the real-world scenario or
safety property it protects, then asserts the externally-visible behaviour.

Run everything:

```bash
cargo test --workspace
```

## Layers

| Layer | Where | What it protects |
|---|---|---|
| **Unit + property** | `nightknight-core` | Unit conversions (round-trip, rounding, mixed mg/dL + mmol/L), the clinical threshold pairs, trend-arrow classification, Time-in-Range / GMI / eA1c maths, ISO-8601 ↔ epoch-ms, and validation that rejects impossible/dangerous values. |
| **Storage contract** | `nightknight-store-sql/src/contract_tests.rs` | The behaviour every backend must share — CRUD, dedup, soft-delete + history, per-user isolation, `lastModified`, token lifecycle. Run against in-memory SQLite; the D1 backend runs the identical SQL. |
| **API contract** | `nightknight-api/tests/api_contract.rs` | End-to-end through `ApiService`: a v1 upload/read round-trip, device-token auth in every form (raw Bearer, raw `api-secret`, legacy SHA-1), query-string credentials being rejected, scope enforcement, v3 dedup + envelope, mixed-unit `current`/`analytics`, and user isolation. |
| **Postgres parity** | `nightknight-store-sql/tests/postgres.rs` | The same storage guarantees run against a **real Postgres** (skipped unless `NK_TEST_PG_URL` is set), proving the container path agrees with SQLite/D1. |
| **Connectors** | `nightknight-connectors` (`dexcom`, `librelinkup`) | Pure protocol logic: request bodies/URLs, the Dexcom `WT` timestamp + trend parsing, the LibreLinkUp login/redirect/connections/graph parsing, the `M/D/YYYY h:mm:ss AM/PM` timestamp, and trend-arrow mapping. |

## Worth highlighting

- **`canonical_clinical_thresholds_round_trip`** (core) pins that 70↔3.9, 180↔10.0,
  54↔3.0 and 250↔13.9 convert exactly — the numbers a person sees daily.
- **`credentials_are_never_accepted_in_the_query_string`** (api) is the executable
  proof of the no-tokens-in-GET rule.
- **`users_are_isolated*`** (storage + api) prove the core privacy property.
- **Property tests** assert conversion is lossless for native mg/dL, stays within ½
  mg/dL on round-trip, and is monotonic — so a mixed-unit chart is always truthful.

Running the Postgres parity test locally:

```bash
docker run --rm -d -p 5433:5432 -e POSTGRES_PASSWORD=pw --name nk-pg postgres:16
NK_TEST_PG_URL='postgres://postgres:pw@localhost:5433/postgres' \
  cargo test -p nightknight-store-sql --test postgres -- --nocapture
docker rm -f nk-pg
```

> This test earned its keep: it caught two real cross-backend bugs that SQLite's
> looseness had hidden — `?` placeholders not being translated to `$n` for Postgres,
> and a text-typed `NULL` bound to a `bigint` column. Both are fixed.

## Manual / live checks

`scripts/dev-preview.sh` runs the server in `dev` auth mode with the SPA; seed it with
a day of synthetic data and open the dashboard (see SETUP.md → Verifying). For
Cloudflare, `wrangler dev` runs the Worker against a local D1.

## Still to add

- Worker integration tests under Miniflare (Access JWT verify paths, routing).
- Realtime push (Durable Object / WebSocket) and its tests.
- iOS unit/UI tests (Phase 2).
