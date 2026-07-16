# Architectural Review — NightKnight

*Review date: 2026-07-16. Scope: the whole repository — Rust service (`service/crates/`,
12 crates), the iOS app (`ios/`) and its FFI consumption, the web SPA (`web/dist/`),
deployment (`deploy/`, `wrangler.toml`), CI (`.github/`), tests, and docs. Read-only
analysis; no runtime behaviour was changed.*

## Verdict

NightKnight is **exceptionally well-architected for its stage**. The central bet — *one
Rust core, two runtimes* — is real, not marketing: the domain core is genuinely pure, the
API is transport-agnostic, storage is a single shared SQL source, and the analytics that
the server, the iOS app (over FFI), and the web all display come from **one** function, so
the three surfaces cannot disagree on a number. The FFI boundary, the error model, and the
multi-tenant isolation are all done to a standard well above what a personal project
usually reaches.

The dominant risk is **not in the design — it is the gap between what the code guarantees
and what CI actually exercises.** The primary production backend (Cloudflare Worker + D1)
and the entire iOS/Swift/web surface pass through CI *untested*. The second theme is a
handful of **insecure-or-fragile defaults** (container auth, migrations, deploy config)
that are fine for the author's own deployment but will bite a third-party self-hoster.

Nothing here is a design dead-end. The recommendations are additive.

---

## What is excellent (preserve these)

- **Clean, acyclic crate DAG with a truly pure core.** `nightknight-core` has zero internal
  dependencies and no I/O; storage depends only on core; backends on storage; the API sits
  above; entrypoints at the top. No cycles, no layering violations.
- **Transport-agnostic API seam.** `ApiService::handle` works over a tiny owned
  `ApiRequest`/`ApiResponse` (`nightknight-api/src/http.rs`), so the identical handlers run
  under Workers and axum and are unit-testable with no server. This is the single strongest
  decision in the codebase.
- **Single-source portable SQL + schema.** Every statement is built once in
  `nightknight-storage/src/sql.rs` with `?` placeholders; both the D1 and sqlx backends
  execute the identical strings and share row-construction helpers. Near-zero
  schema-definition drift by construction.
- **Exemplary FFI boundary** (`nightknight-ffi`, `ios/Shared/RustAnalytics.swift`):
  `catch_unwind` folds panics to in-band `{"error":…}`, one allocator / one `nk_free`,
  inputs copied before any fallible work, an `nk_abi_version()` runtime assert, a
  source-hash staleness guard in CI, and **byte-identical golden tests asserted from both
  Rust and Swift**.
- **Single-source analytics.** `nightknight-core::analytics::report` is the one place the
  v4 wire shape is composed; the server handler and the iOS FFI both call it, and the web
  simply renders its output. Rust↔Swift↔JS analytics cannot drift.
- **Strong multi-tenant isolation.** Every document/token/credential/push query is scoped
  by `user_id`, and the owner is always derived from the authenticated principal, never
  from the request body. Humans and service tokens live in separate namespaces
  (`human:` vs `service:`) so identifiers cannot collide across kinds.
- **Solid auth crypto.** RS256-only JWT verification with signature-before-claims,
  `alg:none`/HS256 rejected, `exp` required, `aud`/`iss` checked, immutable `sub` as the
  tenancy key. Device tokens stored only as hashes; connector credentials sealed with
  AES-256-GCM (per-message random nonce); `Debug` impls redact secrets.
- **Layered `thiserror` error model** with client-detail masking (`error.rs`) and — verified
  — **no `unwrap`/`expect`/`panic!` on any request path** (production `expect`s are
  startup-only fail-fast).
- **A property-tested, unit-aware glucose model** (`units.rs`) — the patient-safety
  centrepiece, with one centralised `18.0156` constant.
- **Readable, intent-stating security tests** (`api_contract.rs`, ~1600 lines) covering
  IDOR/isolation, query-string-credential rejection, service-can't-impersonate-human, scope
  enforcement, and the SSRF guard.

---

## Findings by severity

Severities are the reviewers' judgement of blast radius × likelihood for a **multi-user,
self-hostable health-data** product. File references are to the current tree.

### High

**H1 — CI never builds or tests the production Cloudflare path, the Swift side, or Postgres.**
`.github/workflows/rust.yml` runs only host-target `cargo build`/`cargo test` plus an FFI
source-hash staleness check. Consequences:
- `nightknight-worker` and `nightknight-store-d1` are `#![cfg(target_arch = "wasm32")]`, so
  on the host they compile to **empty** libraries — the D1 backend and Worker glue are never
  compiled or tested. A break there ships green.
- No Xcode job: the **Swift** half of the golden parity (`RustAnalyticsTests.swift`) and all
  iOS test targets never run in CI. The staleness hash proves the artifact matches its
  sources, not that Swift agrees with the golden bytes.
- `postgres.rs` is skipped unless `NK_TEST_PG_URL` is set, and the workflow has no Postgres
  service — the real container DB backend is unverified in CI.
- No `clippy`, no `rustfmt --check` (even though `SETUP.md` documents clippy in the flow).

The "byte-identical across runtimes" claim is **true by construction but only half
CI-backed**. *Fix (mostly a few lines each):* add `cargo build --target
wasm32-unknown-unknown -p nightknight-worker` (ideally `worker-build`); a `services: postgres`
block + `NK_TEST_PG_URL`; `cargo clippy -- -D warnings` and `cargo fmt --check`; and — larger
lift — a macOS runner for the Swift golden tests.

**H2 — Insecure-by-default `trust-header` auth in the container.**
`nightknight-server/src/main.rs` defaults `NK_AUTH_MODE=trust-header` with
`NK_PROXY_SHARED_SECRET` **unset**, and in that state `resolve_edge` trusts
`x-auth-request-email` **unconditionally** — any request carrying
`x-auth-request-email: victim@example.com` is authenticated as that user with full owner
access. `deploy/docker-compose.yml:41` publishes the port on all host interfaces
(`"${NK_PORT:-8787}:8787"`) and `deploy/.env.example` ships `NK_AUTH_MODE=trust-header` with
no secret. If the port is reachable by anything that bypasses the reverse proxy (public/
shared interface, co-tenant, proxy that fails to strip the header), an unauthenticated
attacker impersonates any user and reads/writes their health data. The mitigation exists
(`NK_PROXY_SHARED_SECRET`, constant-time compared, fail-closed) but is off by default; the
server logs a warning and then runs wide open. *Fix:* fail closed unless a shared secret is
configured (or an explicit `insecure-trust-header` opt-in), and bind the example compose
port to `127.0.0.1`.

### Medium

**M1 — D1↔sqlx column-mapping is a live, untested drift vector.** The sqlx backend reads
result columns **by ordinal**; the D1 backend reads **by name** (relying on the `AS day`/
`AS n`/`AS lm` aliases in `sql.rs`). The contract tests run the sqlx/SQLite path only
(H1), so reordering a column list or dropping an alias breaks **only the untested wasm/D1
path** while CI stays green. *Fix:* run the contract suite against D1 (Miniflare), or at
minimum add a name/ordinal column-mapping consistency test.

**M2 — Every write is a two-round-trip check-then-act, on the hot ingest path.** Both
`upsert_document` impls `SELECT` (via `get_document`) before `INSERT … ON CONFLICT` purely
to label `Created` vs `Updated`; no `RETURNING` is used anywhere. On a Nightscout backfill
(`NS_PAGE = 2_000` readings/tick) that is ~4,000 D1 queries per credential per tick, and the
created-flag is a check-then-act race (two concurrent upserts can both report `Created`).
*Fix:* `INSERT … ON CONFLICT … RETURNING` (SQLite ≥3.35, D1, Postgres all support it) or
inspect `rows_affected`, halving ingest query volume.

**M3 — The wasm/D1 float-binding constraint leaks into the "portable" SQL.** Because D1
binds every integer param as a JS `f64`, `daily_counts` must **inline** the tz offset as a
literal and `GROUP BY`/`ORDER BY` on the output ordinal — otherwise SQLite does REAL
arithmetic and the day-bucket explodes to one group per row, OOM-ing the Worker. The rule is
correct and heavily documented, but it lives only in one function's comment and the failure
mode is wasm-only (untested per H1). *Fix:* encode the constraint in the type system (e.g. a
`Param::InlineInt` both backends render literally) so it is discoverable at the call site.

**M4 — `ApiService` is a god-object and `nightknight-api` a god-crate.** One `impl` owns
service config, principal/auth resolution, document CRUD, connector **ingest**, connector
**sync** (`sync_connectors`/`sync_one`/`sync_nightscout`, ~230 lines), and **APNs push** —
which is why `api` depends on `connectors`, `crypto`, and `apns`. Both entrypoints already
call `sync_connectors` from their own schedulers, so the seam is natural. *Fix:* extract a
`nightknight-sync` crate and narrow `ApiService` to request handling + auth + CRUD.

**M5 — No versioned migration mechanism.** Both backends migrate by re-running idempotent
`CREATE TABLE/INDEX IF NOT EXISTS` (`sql.rs`). This safely adds **new tables** but **cannot
evolve an existing table** — adding a column to an existing `CREATE TABLE IF NOT EXISTS` is
a silent no-op on already-deployed databases, and there is no `ALTER`/version/backfill path.
A latent trap that bites the first time a column must be added to `entries`/`users`. *Fix:* a
lightweight ordered-migration runner (`schema_version` row + versioned steps) that slots into
the existing `migrate()` methods, kept portable across D1 and sqlx.

**M6 — License inconsistency.** Every crate declares `license = "AGPL-3.0-or-later"`
(`Cargo.toml` `[workspace.package]`, inherited via `license.workspace = true`), but the
`LICENSE` file and the README badge/section say **MIT**. This is what SBOM tooling,
crates.io, and dependency scanners read, and it contradicts the human-facing license. Given
the project is "inspired by" the AGPL-licensed Nightscout, the correct answer must be
deliberate. *Fix:* reconcile to a single license across `Cargo.toml`, `LICENSE`, and README.

**M7 — Follower credentials stored plaintext in App Group `UserDefaults`, not Keychain
(iOS).** Dexcom/LibreLinkUp passwords and the Nightscout api-secret live in the shared App
Group `UserDefaults` (`ios/Shared/Settings.swift`) — plaintext at rest and in device
backups. This is a *documented* work-around (Keychain access groups don't reliably share
app→widget without provisioning the signing path lacks), and PII in derived keys is hashed
(`accountTag`), but the secrets themselves are not sealed. *Fix:* revisit a properly
provisioned Keychain access group for the secrets (leave non-secret display config in the App
Group), or surface the at-rest/backup exposure to users.

**M8 — Cross-language connector duplication (iOS ↔ Rust).** Dexcom/LibreLinkUp/Nightscout
auth, JSON/timestamp parsing, trend mapping, and the SSRF guard exist twice — Swift
(`ios/Shared/*Client.swift`) and Rust (`nightknight-connectors`) — because the FFI forbids
I/O. Strongly mitigated by 1:1 function naming and **byte-for-byte shared fixtures**
(`ios/Tests/Fixtures`) asserted from both languages. *Highest-leverage fix:* expose the
**pure parsers** (not the I/O) through the FFI (e.g. `nk_parse_dexcom_glucose`), leaving only
the thin `URLSession` edge in Swift. Until then, keep the shared-fixture golden tests
mandatory — but note (H1) they don't run in CI.

**M9 — DNS-rebinding SSRF gap in the Nightscout connector.** `is_safe_base`
(`nightscout.rs`) is a genuinely strong allowlist (https-only, blocks RFC-1918/loopback/
link-local/CGNAT/metadata, strips userinfo, rejects packed IPs, refuses redirects), but it
validates only the literal hostname string. A public DNS name that resolves to an internal
IP passes the guard and reqwest then connects to the internal address (the code comment
acknowledges "not a substitute for network egress controls"). Container-runtime only; no
RFC-1918 network exists on Workers. *Fix:* re-check the resolved IP against the deny-list
before connecting (pinned-IP connector), and/or rely on egress policy; document the residual
risk.

**M10 — Container deploy gaps.** `deploy/Dockerfile` has **no `HEALTHCHECK`** (the server
exposes `/healthz`, unused) and there is **no `.dockerignore`**, so `COPY . .` ships the whole
repo — `.git`, the committed ~25 MB `ios/Rust/*.a` xcframework blobs, `marketing/*.mp4` —
into the build context. `docker-compose.yml` gives the app **no `healthcheck` and no
`restart:` policy** (a monitoring app that should stay up can silently die). *Fix:* add a
`.dockerignore`, a `HEALTHCHECK` hitting `/healthz`, `restart: unless-stopped`, and split
dependency caching so source edits don't rebuild all deps.

**M11 — Documentation drift (under-claims shipped features).** `docs/ARCHITECTURE.md` calls
iOS "Phase 2" and per-user connectors "the next step" (both shipped); its crate graph is also
inaccurate (shows `auth` depending on `core`, which it does not, and omits
`crypto`/`apns`/`ffi` and the real `api` hub edges). `TESTING.md` labels iOS tests "Phase 2"
and omits the FFI golden layer entirely. `PLATFORM-COMPARISON.md` marks silent-APNs and
CarPlay as unbuilt, though both ship. The drift is in the *safe* direction (under-claiming),
but it misleads new contributors. *Fix:* refresh the three docs and regenerate the crate
graph from `cargo tree`.

**M12 — Threshold/band duplication and untested mirror invariants (iOS).**
`GlucoseBand.of` (`ios/Shared/GlucoseUnit.swift`) re-hardcodes 54/70/180/250 independently of
the constants passed to the FFI, and `SourceSetup.Staged.sourceKey`/`isComplete` must
hand-mirror `Settings.sourceKey`/`isConfigured` across two files ("must mirror exactly",
untested). These are manual-lockstep drift surfaces. *Fix:* derive band boundaries from the
same constants; add a table-driven test asserting the source-key invariant for every source.

**M13 — Safety-critical and DTO-mapping iOS code is under-tested.** `AlarmManager.evaluate`
(the out-of-range / fast-drop / throttle logic — the most safety-relevant code in the app)
and `APIClient.mapAnalytics`/`mapAgp` (the ~70-line server-JSON→model drift boundary) have no
unit tests, though both are pure and easily testable. *Fix:* add unit tests for both.

### Low

- **L1 — Postgres password logged at startup.** `main.rs` does
  `tracing::info!(%database_url, …)`, and `database_url` includes the DB password. *Fix:*
  log a redacted URL.
- **L2 — Resilient ingest can return `200 OK` on a storage outage.** `ingest_resilient` and
  `v4_import_csv` fold every failed `store_document` into a "skipped/rejected" count, not
  distinguishing a per-row validation reject from a systemic `StorageError` — a DB outage
  mid-import reports all rows "rejected" with a success status. *Fix:* surface a systemic
  `StorageError` as a real error.
- **L3 — `GET /api/v4/me` and `/status` have no explicit scope gate**, so a read-only
  follower token can read the owner's subject/display name (the account it is already bound
  to — minor info disclosure). Consider a `settings:read` gate for consistency.
- **L4 — Web `innerHTML` bypasses the file's own XSS-safe invariant** in two spots
  (`renderHeroSpark`, `renderCalendar` in `web/dist/app.js`). Today they interpolate only
  numbers, so there is no live XSS, but a future edit that interpolates a real string field
  makes it stored-XSS. *Fix:* build those nodes via the existing `createElementNS` +
  `textContent` path; add a `script-src 'self'` CSP (the SPA has no inline scripts, so it's
  nearly free).
- **L5 — No web JS lint/typecheck.** The "no build step" SPA has no `package.json`, so a
  field-name typo fails silently at runtime. *Fix:* a CI `tsc --noEmit --checkJs` (JSDoc
  types) or ESLint pass — no build output, just a check.
- **L6 — Large files for navigability** (cohesive, not god-functions): `analytics.rs`
  (~1600), `v4.rs` (~1150), and `ios/.../DashboardView.swift` (922 lines, ~10 types incl. all
  of `AnalysisView`). Optional splits improve navigation, not correctness.
- **L7 — Committed personal Cloudflare identifiers.** `wrangler.toml` commits a real
  `database_id` and `CF_TEAM_DOMAIN = "cooney"` (not secrets, but a fork inherits the
  author's account values); a commented placeholder sits right above.
- **L8 — Version drift.** All crates are `0.1.0` (so `/status` reports 0.1.0) while iOS is
  `MARKETING_VERSION = 1.1` and the README says "Alpha".
- **L9 — Committed ~25 MB xcframework + ~12 MB marketing MP4s** in git. The xcframework is a
  deliberate, staleness-guarded trade-off (builds without a Rust toolchain), but it grows
  history on every FFI change; consider building it in a release job instead.
- **L10 — `Storage` trait breadth** (~30 methods across five entity families) and `Postgres
  TEXT/BIGINT` rather than `JSONB`/native booleans — pragmatic today, worth revisiting at
  scale.

---

## Prioritised remediation roadmap

1. **Close the CI gaps (H1).** Highest leverage, mostly a few lines: wasm build + Postgres
   service + clippy/fmt; then a macOS Swift job. This turns the strong in-code guarantees
   into *continuously verified* ones and de-risks M1/M3.
2. **Fix the container auth default (H2).** Fail closed without a shared secret; bind the
   example port to localhost. One-line-ish changes that remove a full multi-tenant bypass for
   third-party self-hosters.
3. **Reconcile the license (M6)** and **redact the DB-URL log (L1)** — trivial, and M6 is a
   correctness/legal issue independent of code.
4. **Introduce versioned migrations (M5)** *before* the first `ALTER` is needed.
5. **Harden storage:** `RETURNING` on upserts (M2), a D1 column-mapping test or Miniflare run
   (M1), the `InlineInt` param type (M3).
6. **Extract `nightknight-sync` (M4)** to slim the API crate/god-object.
7. **iOS: Keychain for secrets (M7), add the alarm + DTO tests (M13), de-duplicate thresholds
   (M12).**
8. **Deploy polish (M10), doc refresh (M11), SSRF resolved-IP check (M9), web CSP + lint
   (L4/L5).**

---

## Method

This review combined a direct read of the architecturally central files (core, API,
storage, auth, FFI, crypto, both runtime entrypoints) with four parallel subsystem
deep-dives (Rust service layering; iOS app + FFI; security & multi-tenancy; web/deploy/CI/
testing). Findings were cross-checked against the source; concrete deployment/security
claims were verified against the referenced files before inclusion.
