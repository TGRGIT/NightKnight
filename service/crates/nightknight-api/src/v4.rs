//! NightKnight's modern **v4** API — the default for first-party clients (web SPA,
//! iOS app).
//!
//! * `GET  /api/v4/status`     — service + the caller's user/unit.
//! * `GET  /api/v4/current`    — latest reading with trend, in both units.
//! * `GET  /api/v4/entries`    — recent readings (`?hours=` / `?count=`).
//! * `GET  /api/v4/analytics`  — the full Statistical-Analysis set over a window:
//!   data sufficiency, Time-in-Range (count + time-weighted), GMI/eA1c, SD/CV, GRI,
//!   time-of-day patterns, episodes, and advanced variability.
//! * `GET  /api/v4/days`       — per-day data coverage + glucose stats (the Data view):
//!   every local day that has readings, its count, and a per-day summary for recent days.
//! * `GET  /api/v4/agp`        — Ambulatory Glucose Profile percentile bands.
//! * `GET/PUT /api/v4/me`      — the caller's profile (preferred unit, name).
//! * `POST/GET /api/v4/tokens`, `DELETE /api/v4/tokens/{id}` — device tokens.
//!
//! Window-based endpoints accept `?tzOffset=<minutes-east-of-UTC>` so time-of-day
//! analytics (AGP, dawn patterns, nocturnal flags) use the caller's local clock.

use serde_json::{json, Value};
use uuid::Uuid;

use nightknight_auth::{Action, Permission, Scope};
use nightknight_core::analytics::export::{self, ExportRange};
use nightknight_core::analytics::report::{self, gap_caps, headline, infer_cadence_ms, tir_json};
use nightknight_core::analytics::{
    self, GlucoseReading, GlucoseSummary, TimeInRange, TirThresholds, DEFAULT_MAX_GAP_MS,
};
use nightknight_core::documents::Entry;
use nightknight_core::import::{parse_glucose_csv, DateOrder};
use nightknight_core::timeutil;
use nightknight_core::trend::{self, Direction};
use nightknight_core::units::GlucoseUnit;
use nightknight_storage::{
    Collection, ConnectorCredential, DayCount, DeviceToken, DocQuery, PushToken, Storage, StoredDoc,
};

use super::{ApiError, ApiRequest, ApiResponse, ApiService, EdgeIdentity, Principal};
use crate::hashing::{legacy_hash, token_hash};
use crate::http::Method;
use crate::{SERVICE_NAME, SERVICE_VERSION};

const DEFAULT_HOURS: i64 = 24;
const MAX_ANALYTICS_POINTS: i64 = 20_000;
/// Default export window when the caller gives no `start`/`end` — the AGP-standard 14
/// days, so a bare `GET /api/v4/export` yields the report a clinician expects.
const DEFAULT_EXPORT_DAYS: i64 = 14;
/// Hard ceiling on an export window's span. Matches the 90-day AGP standard so an
/// unbounded range can't blow the per-request compute budget; a wider request is
/// clamped to the most-recent 90 days.
const MAX_EXPORT_DAYS: i64 = 90;
/// Rows per D1/Postgres query in the paginated **CSV** export fetch. Keeping each
/// subrequest small (~5k rows ≈ a few MB payload) is what lets a raw export succeed on
/// the Cloudflare Worker without blowing the per-request CPU or D1 payload limits — a
/// single unbounded `search_documents` for 30k+ rows exceeds the Worker's budget.
const EXPORT_BATCH_SIZE: i64 = 5_000;
/// Absolute per-request ceiling on the **raw CSV** export. A raw dump can't be
/// downsampled (it's the verbatim readings), so a very dense window is still bounded and
/// the file marks itself `truncated: true` when the cap is hit. The aggregated JSON/report
/// path has no such cap — it downsamples server-side (see `EXPORT_TARGET_SAMPLES`) and
/// always covers the whole window.
const MAX_EXPORT_POINTS: i64 = 100_000;
/// Target sample count for the **aggregated JSON/report** export. The server picks a
/// whole-minute bucket width so a window of any density collapses to about this many
/// representative readings via `Storage::downsampled_documents` — plenty for faithful
/// AGP/analytics (hundreds–thousands of points per time-of-day bin across a 90-day
/// report) while bounding the Worker's fetch + compute so it never 503s on dense data.
const EXPORT_TARGET_SAMPLES: i64 = 40_000;
/// Floor on the downsample bucket width: one minute. Finer than clinical CGM resolution,
/// so normal-cadence data (1–5 min) passes through un-thinned; only sub-minute-dense
/// sources (merged feeds, raw xDrip) get collapsed to one reading per minute.
const MIN_EXPORT_BUCKET_MS: i64 = 60_000;
/// How many recent readings `current` pulls to estimate a fallback trend — enough to
/// span the 15-minute regression window at 5-minute cadence with headroom.
const CURRENT_TREND_POINTS: i64 = 8;
/// If the latest reading is older than this, the trend is stale and we report no arrow
/// rather than a misleading one. The research staleness basis is ~2× the source's
/// cadence (≈10–11 min at the 5-minute Dexcom cadence); we allow a small grace for one
/// late sample. The reading *value* itself is always still returned. (Distinct from the
/// 15-minute regression window — this guards the latest sample's freshness, not span.)
const TREND_STALE_MS: i64 = 12 * 60_000;
/// How many readings the `/days` view loads **per batch** when computing per-day glucose
/// stats (mean / TIR / uGMI / min-max). The day *list* — every local day that has data,
/// with its reading count — always comes from the cheap `daily_counts` aggregation
/// regardless of this cap, so coverage stays complete across thousands of days; only the
/// richer per-day glucose summary is loaded in batches. Kept at the same ceiling as
/// `MAX_ANALYTICS_POINTS` (20k), which is proven to load fine from D1.
const MAX_DAYS_STATS_POINTS: i64 = 20_000;
/// How many `MAX_DAYS_STATS_POINTS`-sized batches `/days` will load to decorate days,
/// newest first. The per-day summary walks history in **day-aligned** batches and drops
/// each batch's readings before loading the next, so peak memory stays at one batch while
/// coverage scales to the whole history. A flat reading cap used to load only the newest
/// ~20k readings — with dense 1-minute data that is barely two weeks, so every older day
/// showed a count with no average. This budget (12 × 20k = 240k readings) covers years of
/// 5-minute data, or ~5 months of 1-minute data, before any day falls back to count-only.
const MAX_DAYS_STATS_BATCHES: usize = 12;

impl<S: Storage> ApiService<S> {
    pub(crate) async fn route_v4(
        &self,
        req: &ApiRequest,
        now_ms: i64,
        edge: Option<EdgeIdentity>,
        tail: &[&str],
    ) -> Result<ApiResponse, ApiError> {
        let principal = self.resolve_principal(req, edge, now_ms).await?;
        match (req.method, tail) {
            (Method::Get, ["status"]) => self.v4_status(&principal),
            (Method::Get, ["current"]) => self.v4_current(&principal, now_ms).await,
            (Method::Get, ["entries"]) => self.v4_entries(req, &principal, now_ms).await,
            (Method::Get, ["analytics"]) => self.v4_analytics(req, &principal, now_ms).await,
            (Method::Get, ["days"]) => self.v4_days(req, &principal).await,
            (Method::Get, ["agp"]) => self.v4_agp(req, &principal, now_ms).await,
            (Method::Get, ["export"]) => self.v4_export(req, &principal, now_ms).await,
            // `csv` auto-detects the exporter; `libreview` is kept as a back-compat alias.
            (Method::Post, ["import", "csv"] | ["import", "libreview"]) => {
                self.v4_import_csv(req, &principal, now_ms).await
            }
            (Method::Get, ["me"]) => self.v4_me(&principal),
            (Method::Put, ["me"]) => self.v4_update_me(req, &principal).await,
            (Method::Get, ["tokens"]) => self.v4_list_tokens(&principal).await,
            (Method::Post, ["tokens"]) => self.v4_create_token(req, &principal, now_ms).await,
            (Method::Delete, ["tokens", id]) => self.v4_revoke_token(&principal, id).await,
            (Method::Get, ["connectors"]) => self.v4_list_connectors(&principal).await,
            (Method::Put, ["connectors", provider]) => {
                self.v4_put_connector(req, &principal, provider, now_ms).await
            }
            (Method::Delete, ["connectors", provider]) => {
                self.v4_delete_connector(&principal, provider).await
            }
            (Method::Post, ["push", "register"]) => {
                self.v4_push_register(req, &principal, now_ms).await
            }
            (Method::Delete, ["push", "register"]) => {
                self.v4_push_unregister(req, &principal).await
            }
            _ => Err(ApiError::NotFound),
        }
    }

    fn v4_status(&self, principal: &Principal) -> Result<ApiResponse, ApiError> {
        Ok(ApiResponse::json(
            200,
            &json!({
                "name": SERVICE_NAME,
                "version": SERVICE_VERSION,
                "user": user_json(principal),
            }),
        ))
    }

    /// Latest reading + trend, expressed in both units so the client can show either.
    ///
    /// Trend is taken from the **sensor's own** arrow when the latest entry carries one
    /// (Dexcom Share / LibreLinkUp report it, and the connectors persist it onto the
    /// entry's `direction`) — that figure comes from the transmitter's unfiltered,
    /// higher-cadence data and is authoritative. When absent (e.g. a manual entry or a
    /// source with no trend) we fall back to a least-squares estimate over the last
    /// 15 minutes, which is far steadier than a two-point delta.
    async fn v4_current(&self, principal: &Principal, now_ms: i64) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let docs = self
            .storage
            .search_documents(
                Collection::Entries,
                &principal.user.id,
                &DocQuery::new().doc_type("sgv").limit(CURRENT_TREND_POINTS),
            )
            .await?;
        let readings: Vec<GlucoseReading> = docs.iter().filter_map(reading_from_doc).collect();
        let Some(latest) = readings.first().copied() else {
            return Ok(ApiResponse::json(200, &json!({ "current": Value::Null })));
        };
        // A stale latest reading gets no arrow — a two-hour-old "Steady" would mislead.
        let direction = if now_ms - latest.date_ms > TREND_STALE_MS {
            Direction::None
        } else {
            // Prefer the sensor's first-party arrow on the newest entry (docs are
            // newest-first); only fall back to our own estimate when it has none.
            match docs.first().and_then(direction_from_doc) {
                Some(d) if d.is_arrow() => d,
                _ => trend::classify_recent(&readings),
            }
        };
        let g = latest.value;
        // The glucose **level** band (Urgent low … Urgent high) is a separate dimension
        // from the **trend** arrow; clients show both. Computed here so web/iOS/watch
        // share one source of truth and one vocabulary.
        let band = TirThresholds::default().band(g.mgdl());
        Ok(ApiResponse::json(
            200,
            &json!({
                "current": {
                    "date": latest.date_ms,
                    "dateString": timeutil::to_iso8601_ms(latest.date_ms),
                    "mgdl": g.mgdl_rounded(),
                    "mmol": g.display(GlucoseUnit::Mmol),
                    "direction": direction.name(),
                    "trend": direction.arrow(),
                    "trendLabel": direction.label(),
                    "level": band.key(),
                    "levelLabel": band.label(),
                    "preferredUnit": principal.user.preferred_unit,
                }
            }),
        ))
    }

    async fn v4_entries(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let (since, limit) = window(req, now_ms);
        let docs = self
            .storage
            .search_documents(
                Collection::Entries,
                &principal.user.id,
                &DocQuery::new().doc_type("sgv").date_gte(since).limit(limit),
            )
            .await?;
        let points: Vec<Value> = docs
            .iter()
            .filter_map(|d| {
                reading_from_doc(d).map(|r| {
                    json!({
                        "date": r.date_ms,
                        "mgdl": r.value.mgdl_rounded(),
                        "mmol": r.value.display(GlucoseUnit::Mmol),
                    })
                })
            })
            .collect();
        Ok(ApiResponse::json(200, &json!({ "entries": points })))
    }

    /// The full Statistical-Analysis set over a window (default 24h). Backward
    /// compatible: every field the original payload carried is still present, with the
    /// deeper metrics added alongside. `?tzOffset=` (minutes east of UTC) localises the
    /// time-of-day analytics and the nocturnal-episode flag.
    ///
    /// The payload itself is assembled by [`report::analytics_value`] in
    /// `nightknight-core` — shared verbatim with the iOS on-device FFI, so the two can
    /// never drift.
    async fn v4_analytics(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let hours = req.query_int("hours").unwrap_or(DEFAULT_HOURS).clamp(1, 24 * 90);
        let tz = tz_offset(req);
        let since = now_ms - hours * 3_600_000;
        let docs = self
            .storage
            .search_documents(
                Collection::Entries,
                &principal.user.id,
                &DocQuery::new()
                    .doc_type("sgv")
                    .date_gte(since)
                    .limit(MAX_ANALYTICS_POINTS),
            )
            .await?;
        let readings: Vec<GlucoseReading> = docs.iter().filter_map(reading_from_doc).collect();
        let t = TirThresholds::default();
        Ok(ApiResponse::json(200, &report::analytics_value(&readings, hours, tz, &t)))
    }

    /// Per-day data coverage + glucose stats for the **Data** view — the answer to "did
    /// my history actually import, and what does each day look like?".
    ///
    /// Two tiers, by design, so it scales to thousands of days:
    /// * **Every** local day that has ≥1 sgv reading is listed with its reading `count`,
    ///   first/last reading time, and an `expectedPerDay` derived from *that day's own*
    ///   cadence (see [`day_expected_per_day`]) — so a complete day from a slower-sensor
    ///   era isn't mislabelled under-covered against a faster recent cadence. All from the
    ///   cheap indexed [`Storage::daily_counts`] aggregation (no document bodies loaded).
    /// * Days additionally get a per-day glucose summary (mean, TIR, uGMI/GMI, CV,
    ///   min/max), computed by walking history newest-first in **day-aligned batches** of
    ///   up to [`MAX_DAYS_STATS_POINTS`] readings (see [`collect_day_glucose_stats`]). The
    ///   walk drops each batch's readings before loading the next, so peak memory stays at
    ///   one batch while coverage scales to the whole history (bounded by
    ///   [`MAX_DAYS_STATS_BATCHES`]); only days beyond that budget fall back to count-only.
    ///
    /// `?tzOffset=` (minutes east of UTC) sets the local day boundary so days line up
    /// with the caller's calendar.
    async fn v4_days(
        &self,
        req: &ApiRequest,
        principal: &Principal,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let tz = tz_offset(req);
        let tz_ms = tz * 60_000;

        // 1) Every day that has data + its count (cheap; scales to thousands of days).
        let counts = self
            .storage
            .daily_counts(Collection::Entries, &principal.user.id, "sgv", tz_ms)
            .await?;

        // 2) Per-day glucose summaries, loaded in day-aligned batches across the full
        //    history (newest first) so dense data doesn't clip older days to count-only.
        let t = TirThresholds::default();
        let stats = self
            .collect_day_glucose_stats(
                &principal.user.id,
                &counts,
                tz,
                MAX_DAYS_STATS_POINTS,
                MAX_DAYS_STATS_BATCHES,
            )
            .await?;

        let total_readings: i64 = counts.iter().map(|d| d.n).sum();
        let cadence_ms = stats.cadence_ms;
        let (tw_gap, _) = gap_caps(cadence_ms);
        let expected_per_day = (timeutil::DAY_MS as f64 / cadence_ms as f64).round() as i64;

        let days: Vec<Value> = counts
            .iter()
            .map(|d| {
                let day_stats = stats.by_day.get(&d.day_index);
                // Coverage is judged against THIS day's own sampling cadence, not one
                // global rate — so a genuinely complete day from a slower-sensor era (e.g.
                // 5-min, n≈288) isn't mislabelled under-covered against a faster recent
                // cadence (e.g. 1-min, ≈1440/day). The web client divides `n` by this.
                // Fully-loaded days carry the expectation from their own median gap; days
                // beyond the batch budget estimate it from their span (see `day_stats`).
                let expected = day_stats
                    .map(|s| s.expected_per_day)
                    .unwrap_or_else(|| day_expected_per_day(d, None, cadence_ms, tz));
                let mut obj = json!({
                    "date": timeutil::date_string_from_day_number(d.day_index),
                    "dayIndex": d.day_index,
                    "n": d.n,
                    "firstMs": d.first_ms,
                    "lastMs": d.last_ms,
                    "expectedPerDay": expected,
                });
                // Attach the per-day glucose summary for every day the batched walk fully
                // loaded. Days beyond the batch budget (oldest history) carry the count
                // only — the UI shows that honestly via `statsCapped`.
                if let (Some(s), Value::Object(m)) = (day_stats, &mut obj) {
                    m.insert("meanMgdl".into(), json!(s.mean));
                    m.insert("minMgdl".into(), json!(s.min));
                    m.insert("maxMgdl".into(), json!(s.max));
                    m.insert("uGmiPercent".into(), json!(s.ugmi));
                    m.insert("gmiPercent".into(), json!(s.gmi));
                    m.insert("cvPercent".into(), json!(s.cv));
                    m.insert("timeInRange".into(), tir_json(&s.tir));
                }
                obj
            })
            .collect();

        // Headline stats over the most recent batch (the UI labels these "recent"),
        // time-weighted for the same non-uniform-sampling robustness as /analytics.
        let w = GlucoseSummary::compute(&stats.recent_readings, &t);
        let wh = headline(&w, &stats.recent_readings, tw_gap);

        Ok(ApiResponse::json(
            200,
            &json!({
                "tzOffset": tz,
                "totalDays": counts.len(),
                "totalReadings": total_readings,
                "firstDay": counts.last().map(|d| timeutil::date_string_from_day_number(d.day_index)),
                "lastDay": counts.first().map(|d| timeutil::date_string_from_day_number(d.day_index)),
                "cadenceMs": cadence_ms,
                // Global default cadence (most recent batch). Each day also carries its own
                // `expectedPerDay`; this top-level value is the client's fallback only.
                "expectedPerDay": expected_per_day,
                "statsWindowReadings": stats.loaded,
                "statsCapped": stats.capped,
                "windowStats": {
                    "n": w.n,
                    "meanMgdl": wh.mean,
                    "uGmiPercent": wh.ugmi,
                    "gmiPercent": wh.gmi,
                    "estimatedA1cPercent": wh.ea1c,
                    "cvPercent": wh.cv,
                    "timeInRange": tir_json(&w.tir),
                },
                "days": days,
            }),
        ))
    }

    /// Walk a user's sgv history newest-first in **day-aligned batches**, computing a
    /// per-day glucose summary for every day that fits inside the batch budget. Returns the
    /// per-day decorations, the most-recent batch's readings (for the "recent" window
    /// headline + cadence), how many readings were loaded, and whether the budget capped
    /// older days to count-only.
    ///
    /// Why batches rather than one capped load: a single `LIMIT 20_000` query returns only
    /// the newest ~20k readings, which at a 1-minute cadence is barely two weeks — so every
    /// older day showed a reading count with no average. Each batch here groups *whole
    /// days* (using the indexed first/last-ms bounds from `daily_counts`, so a day is never
    /// split across batches) and is dropped before the next loads, keeping peak memory at
    /// one batch while coverage extends across the whole history.
    ///
    /// `counts` must be the [`Storage::daily_counts`] result (one entry per day with data,
    /// **newest day first**). `tz` is minutes east of UTC; `page_size` bounds readings per
    /// batch; `max_batches` bounds total work.
    async fn collect_day_glucose_stats(
        &self,
        user_id: &str,
        counts: &[DayCount],
        tz: i64,
        page_size: i64,
        max_batches: usize,
    ) -> Result<DayGlucoseStats, ApiError> {
        let t = TirThresholds::default();
        let mut by_day: std::collections::HashMap<i64, DayGlucose> = std::collections::HashMap::new();
        let mut recent_readings: Vec<GlucoseReading> = Vec::new();
        let mut loaded: i64 = 0;
        let mut capped = false;
        // Cadence/gap caps are inferred once from the most recent batch (representative of
        // the device) and reused for every day's time-weighted headline, matching the old
        // single-window behaviour. Defaults until the first batch is loaded.
        let mut cadence_ms = analytics::DEFAULT_CADENCE_MS;
        let mut tw_gap = DEFAULT_MAX_GAP_MS;

        let mut i = 0usize;
        let mut batches = 0usize;
        while i < counts.len() {
            if batches >= max_batches {
                capped = true;
                break;
            }
            // Build a day-aligned batch: consecutive days (newest first) whose summed
            // reading count fits one page, always taking at least one day.
            let start = i;
            let mut sum = 0i64;
            while i < counts.len() {
                let n = counts[i].n;
                if i > start && sum + n > page_size {
                    break;
                }
                sum += n;
                i += 1;
                if sum >= page_size {
                    break;
                }
            }
            let batch = &counts[start..i];
            // A single day bigger than one page can't be fully loaded within the per-batch
            // bound; leave it count-only (honest) rather than decorate from a partial slice.
            if batch.len() == 1 && batch[0].n > page_size {
                capped = true;
                continue;
            }

            let newest = &batch[0]; // counts is newest-day-first
            let oldest = &batch[batch.len() - 1];
            let docs = self
                .storage
                .search_documents(
                    Collection::Entries,
                    user_id,
                    &DocQuery::new()
                        .doc_type("sgv")
                        .date_gte(oldest.first_ms)
                        .date_lte(newest.last_ms)
                        .limit(sum),
                )
                .await?;
            batches += 1;
            loaded += docs.len() as i64;
            let readings: Vec<GlucoseReading> = docs.iter().filter_map(reading_from_doc).collect();

            // The first (most recent) batch sets the cadence used for every day's headline,
            // and is the "recent" window the summary tiles report.
            if batches == 1 {
                cadence_ms = infer_cadence_ms(&readings, tz);
                tw_gap = gap_caps(cadence_ms).0;
            }

            // Group this batch's readings by local day and decorate each fully-loaded day.
            let mut grouped: std::collections::HashMap<i64, Vec<GlucoseReading>> =
                std::collections::HashMap::new();
            for r in &readings {
                grouped.entry(timeutil::day_number(r.date_ms, tz)).or_default().push(*r);
            }
            for d in batch {
                if let Some(rs) = grouped.get(&d.day_index) {
                    // Decorate only when the whole day loaded (loaded == authoritative
                    // count) — a partial day's mean/TIR alongside the full `n` would mislead.
                    if rs.len() as i64 == d.n {
                        // This day's expectation comes from its OWN cadence (median gap),
                        // not the global one, so a complete slower-sensor day reads as fully
                        // covered. The global `cadence_ms` is only the lone-reading fallback.
                        let expected = day_expected_per_day(d, Some(rs), cadence_ms, tz);
                        by_day.insert(d.day_index, DayGlucose::compute(rs, &t, tw_gap, expected));
                    }
                }
            }

            if batches == 1 {
                recent_readings = readings;
            }
        }
        if i < counts.len() {
            capped = true;
        }

        Ok(DayGlucoseStats { by_day, recent_readings, loaded, capped, cadence_ms })
    }

    /// Ambulatory Glucose Profile: the 5/25/50/75/95 percentile bands of glucose by
    /// time of day, every day in the window overlaid onto one 24-hour axis. `?days=`
    /// (default 14), `?bin=` minutes (default 15 → 96 bins), `?tzOffset=` minutes.
    async fn v4_agp(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let days = req.query_int("days").unwrap_or(14).clamp(1, 90);
        let bin = req.query_int("bin").unwrap_or(15).clamp(5, 60);
        let tz = tz_offset(req);
        let since = now_ms - days * 24 * 3_600_000;
        let docs = self
            .storage
            .search_documents(
                Collection::Entries,
                &principal.user.id,
                &DocQuery::new().doc_type("sgv").date_gte(since).limit(MAX_ANALYTICS_POINTS),
            )
            .await?;
        let readings: Vec<GlucoseReading> = docs.iter().filter_map(reading_from_doc).collect();
        Ok(ApiResponse::json(200, &report::agp_value(&readings, days, bin, tz)))
    }

    /// Export the caller's readings over a date range, in a machine-readable format, as a
    /// downloadable file (`Content-Disposition: attachment`).
    ///
    /// * `?format=csv` → the raw `sgv` readings as CSV (one row per reading), for a
    ///   spreadsheet or re-import.
    /// * `?format=json` (the default) → the full computed metric set: the `/analytics`
    ///   payload plus the AGP percentile bands, wrapped with the date range and generation
    ///   timestamp — the data behind a printable AGP one-pager.
    ///
    /// The window is `?start=`/`?end=` epoch ms (aliases `?from=`/`?to=`); it defaults to
    /// the last 14 days and is clamped to a 90-day span (the analytics ceiling). `?tzOffset=`
    /// (minutes east of UTC) localises the timestamps and the AGP/time-of-day maths;
    /// `?bin=` sets the AGP bin width for the JSON export. Every artefact is labelled with
    /// the range + generation time inside the file itself.
    ///
    /// Authorization is `entries:read` and the query is scoped to `principal.user.id`, so
    /// an export can only ever contain the authenticated caller's own readings.
    async fn v4_export(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let tz = tz_offset(req);

        // Resolve the window. Accept both the `start`/`end` and the `from`/`to` spellings.
        // Use saturating arithmetic throughout so a client passing an extreme `end`/`start`
        // near the i64 rails can't overflow the span/default-start math into a bogus window.
        let end = req.query_int("end").or_else(|| req.query_int("to")).unwrap_or(now_ms);
        let default_start = end.saturating_sub(DEFAULT_EXPORT_DAYS * timeutil::DAY_MS);
        let mut start = req.query_int("start").or_else(|| req.query_int("from")).unwrap_or(default_start);
        // Clamp the span so an unbounded range can't exceed the point cap / compute budget:
        // keep the most-recent MAX_EXPORT_DAYS, and never let start run past end.
        let max_span = MAX_EXPORT_DAYS * timeutil::DAY_MS;
        if end.saturating_sub(start) > max_span {
            start = end.saturating_sub(max_span);
        }
        if start > end {
            start = end;
        }

        let t = TirThresholds::default();

        // `format` defaults to json; anything else is a client error, not a silent fallback.
        match req.query_get("format").unwrap_or("json") {
            "csv" => {
                // Raw dump: paginated cursor-walk in `EXPORT_BATCH_SIZE`-row batches, up to
                // `MAX_EXPORT_POINTS`. A raw export can't be downsampled (it's the verbatim
                // readings), so a very dense window is bounded and the file self-marks
                // `truncated`. Each small batch stays under the Worker's per-request budget.
                let (readings, truncated) = self
                    .fetch_export_readings(&principal.user.id, start, end)
                    .await?;
                let range =
                    ExportRange { start_ms: start, end_ms: end, generated_ms: now_ms, tz, truncated };
                let body = export::readings_csv(&readings, &range).into_bytes();
                let name = format!("{}.csv", export::filename_stem("readings", &range));
                Ok(ApiResponse::bytes(200, "text/csv; charset=utf-8", body)
                    .with_header("content-disposition", attachment(&name)))
            }
            "json" => {
                // Aggregate over the WHOLE window via a server-side downsample: one
                // representative reading per adaptive whole-minute bucket (an index-only
                // `GROUP BY mills` in storage), so a 90-day report covers every day
                // regardless of source density and never 503s. The bucket widens only as
                // far as needed to keep the sample count near `EXPORT_TARGET_SAMPLES`, so
                // normal 1–5-min data passes through un-thinned. The fetch is paginated in
                // `EXPORT_BATCH_SIZE` batches (like the CSV path) so no single subrequest
                // materialises more than that many document bodies.
                let window_ms = (end - start).max(1);
                let bucket_ms = downsample_bucket_ms(window_ms);
                let readings = self
                    .fetch_downsampled_readings(&principal.user.id, start, end, bucket_ms)
                    .await?;
                let range = ExportRange {
                    start_ms: start,
                    end_ms: end,
                    generated_ms: now_ms,
                    tz,
                    truncated: false,
                };
                let bin =
                    req.query_int("bin").unwrap_or(export::DEFAULT_AGP_BIN_MINUTES).clamp(5, 60);
                let mut value = export::metrics_json(&readings, &range, bin, &t);
                // Record the downsample resolution so a reader knows the metrics reflect
                // one reading per `sampleBucketMs`, and (on request) attach the compact
                // sample series the printable report draws its daily-profile thumbnails
                // from — so the report needs no second, unbounded `/entries` fetch.
                let want_samples = req
                    .query_get("samples")
                    .is_some_and(|s| s == "1" || s.eq_ignore_ascii_case("true"));
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("sampleBucketMs".into(), json!(bucket_ms));
                    if want_samples {
                        let mut ordered: Vec<&GlucoseReading> = readings.iter().collect();
                        ordered.sort_by_key(|r| r.date_ms);
                        // Compact `[epoch_ms, mg_dL]` pairs, oldest first.
                        let arr: Vec<Value> = ordered
                            .iter()
                            .map(|r| json!([r.date_ms, r.value.mgdl_rounded()]))
                            .collect();
                        obj.insert("samples".into(), json!(arr));
                    }
                }
                // Pretty-print the lean metrics-only download; serialise the report variant
                // (with its thousands of sample pairs) compactly.
                let body = if want_samples {
                    serde_json::to_vec(&value)
                } else {
                    serde_json::to_vec_pretty(&value)
                }
                .unwrap_or_else(|_| b"{}".to_vec());
                let name = format!("{}.json", export::filename_stem("metrics", &range));
                Ok(ApiResponse::bytes(200, "application/json; charset=utf-8", body)
                    .with_header("content-disposition", attachment(&name)))
            }
            other => Err(ApiError::BadRequest(format!(
                "unknown export format '{other}' (use csv or json)"
            ))),
        }
    }

    /// Pull the caller's sgv readings in `[start, end]` from storage in bounded batches
    /// so a large window can't blow the Cloudflare Worker's per-request CPU or D1's per-
    /// query payload limits (a single unbounded fetch for 30k+ rows 503s the Worker).
    ///
    /// Walks backward from `end`: each iteration asks for at most `EXPORT_BATCH_SIZE`
    /// rows with `mills ≤ cursor`, appends them, then steps the cursor to
    /// `oldest.mills - 1` for the next batch. Stops when the window drains (a short
    /// batch), when the total reaches `MAX_EXPORT_POINTS` (returns `truncated = true`),
    /// or when the cursor crosses `start`. Storage returns rows newest-first, so the
    /// returned vector is newest-first too — the CSV/JSON producers sort or re-anchor
    /// as needed. Readings that don't parse (impossibly-old / bad-value rows) are
    /// silently dropped, matching the other analytics endpoints.
    async fn fetch_export_readings(
        &self,
        user_id: &str,
        start: i64,
        end: i64,
    ) -> Result<(Vec<GlucoseReading>, bool), ApiError> {
        let mut readings: Vec<GlucoseReading> = Vec::new();
        let mut cursor_end = end;
        let mut truncated = false;
        // A safety fuse on the loop itself: `MAX_EXPORT_POINTS / EXPORT_BATCH_SIZE`
        // full batches, plus a few extra for the final short one. Prevents an
        // adversarial storage backend that never advances the cursor from spinning
        // forever, without changing any legitimate behaviour.
        let max_batches = (MAX_EXPORT_POINTS / EXPORT_BATCH_SIZE) as usize + 4;
        for _ in 0..max_batches {
            let remaining = MAX_EXPORT_POINTS - readings.len() as i64;
            if remaining <= 0 {
                truncated = true;
                break;
            }
            if cursor_end < start {
                break;
            }
            let batch_size = remaining.min(EXPORT_BATCH_SIZE);
            let docs = self
                .storage
                .search_documents(
                    Collection::Entries,
                    user_id,
                    &DocQuery::new()
                        .doc_type("sgv")
                        .date_gte(start)
                        .date_lte(cursor_end)
                        .limit(batch_size),
                )
                .await?;
            if docs.is_empty() {
                break;
            }
            let batch_len = docs.len() as i64;
            // Rows are ordered mills DESC; the LAST row is the oldest of the batch and
            // the anchor for the next cursor step. Subtract 1 so `date_lte` (inclusive)
            // doesn't re-select the same document; ms-level collisions between distinct
            // sgv readings don't happen in practice.
            let oldest_mills = docs.last().map(|d| d.mills).unwrap_or(cursor_end);
            for d in &docs {
                if let Some(r) = reading_from_doc(d) {
                    readings.push(r);
                }
            }
            if batch_len < batch_size {
                // A short batch means the window is drained — no more rows to fetch.
                break;
            }
            cursor_end = oldest_mills.saturating_sub(1);
        }
        Ok((readings, truncated))
    }

    /// Pull a **downsampled** series for the aggregated JSON/report export: one reading per
    /// `bucket_ms` time-bucket across `[start, end]`, newest first, covering the whole
    /// window. Paginated in `EXPORT_BATCH_SIZE` batches (like [`fetch_export_readings`]) so
    /// no single storage query materialises more than that many document bodies — the JSON
    /// path holds the same per-subrequest budget as the raw CSV path, and a 90-day dense
    /// window can't 503 the Worker.
    ///
    /// Buckets are absolute (`mills / bucket_ms`), so shrinking the `end` cursor each batch
    /// walks a prefix of the same bucket set with no gaps or overlaps. Ties are de-duped by
    /// bucket index (two documents sharing a millisecond that is a bucket minimum both match
    /// storage's group-min `IN`-set; we keep the first and drop the rest), guaranteeing
    /// exactly one reading per occupied bucket regardless of duplicate timestamps.
    async fn fetch_downsampled_readings(
        &self,
        user_id: &str,
        start: i64,
        end: i64,
        bucket_ms: i64,
    ) -> Result<Vec<GlucoseReading>, ApiError> {
        let bucket_ms = bucket_ms.max(1);
        let mut readings: Vec<GlucoseReading> = Vec::new();
        let mut seen_buckets: std::collections::HashSet<i64> = std::collections::HashSet::new();
        let mut cursor_end = end;
        // The representative set is bounded by `window / bucket ≤ EXPORT_TARGET_SAMPLES`, so
        // ~`target / batch` batches drain it; the ×4 slack absorbs tie-inflated batches
        // (duplicate mills add rows without adding buckets) and the final short batch, and
        // the whole bound fuses a backend that never advances the cursor.
        let max_batches = (EXPORT_TARGET_SAMPLES / EXPORT_BATCH_SIZE) as usize * 4 + 8;
        for _ in 0..max_batches {
            if cursor_end < start {
                break;
            }
            let docs = self
                .storage
                .downsampled_documents(
                    Collection::Entries,
                    user_id,
                    "sgv",
                    start,
                    cursor_end,
                    bucket_ms,
                    Some(EXPORT_BATCH_SIZE),
                )
                .await?;
            if docs.is_empty() {
                break;
            }
            let batch_len = docs.len() as i64;
            let oldest_mills = docs.last().map(|d| d.mills).unwrap_or(cursor_end);
            for d in &docs {
                // One reading per bucket: on a tie at a bucket minimum, `downsampled_documents`
                // can return more than one row for the same bucket — keep the first seen.
                if seen_buckets.insert(d.mills / bucket_ms) {
                    if let Some(r) = reading_from_doc(d) {
                        readings.push(r);
                    }
                }
            }
            if batch_len < EXPORT_BATCH_SIZE {
                // A short batch means every remaining bucket in the window is drained.
                break;
            }
            cursor_end = oldest_mills.saturating_sub(1);
        }
        Ok(readings)
    }

    /// Import a glucose CSV export (the raw CSV is the request body) into the caller's
    /// own account. The format — LibreView or Dexcom Clarity — is auto-detected from the
    /// header. `?tzOffset=` (minutes east of UTC) anchors the export's local timestamps;
    /// `?dateOrder=mdy|dmy` overrides LibreView's day/month auto-detection (ignored for
    /// Dexcom, whose timestamps are unambiguous). Every parsed reading goes through the
    /// normal validated, content-deduped write path, so re-importing overlapping data
    /// updates rather than duplicates points.
    async fn v4_import_csv(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Create))?;
        let tz = tz_offset(req);
        let order = match req.query_get("dateOrder") {
            Some(s) if s.eq_ignore_ascii_case("dmy") || s.eq_ignore_ascii_case("day") => {
                Some(DateOrder::DayFirst)
            }
            Some(s) if s.eq_ignore_ascii_case("mdy") || s.eq_ignore_ascii_case("month") => {
                Some(DateOrder::MonthFirst)
            }
            _ => None,
        };
        let text = std::str::from_utf8(&req.body)
            .map_err(|_| ApiError::BadRequest("CSV body must be UTF-8 text".into()))?;
        let parsed =
            parse_glucose_csv(text, tz, order).map_err(|e| ApiError::BadRequest(e.to_string()))?;

        // Ingest resiliently: a single implausible row (bad timestamp/glucose) is
        // counted and skipped, never aborting the whole import.
        let (mut imported, mut duplicates, mut rejected) = (0usize, 0usize, 0usize);
        for entry in parsed.entries {
            match self.store_document(Collection::Entries, entry, principal, now_ms).await {
                Ok(o) if o.created() => imported += 1,
                Ok(_) => duplicates += 1,
                Err(_) => rejected += 1,
            }
        }
        Ok(ApiResponse::json(
            200,
            &json!({
                "source": parsed.source,
                "unit": parsed.unit,
                "dateOrder": match parsed.order {
                    DateOrder::MonthFirst => "mdy",
                    DateOrder::DayFirst => "dmy",
                },
                "rows": parsed.rows,
                "parsed": parsed.imported,
                "skippedRows": parsed.skipped,
                "imported": imported,
                "duplicates": duplicates,
                "rejected": rejected,
            }),
        ))
    }

    fn v4_me(&self, principal: &Principal) -> Result<ApiResponse, ApiError> {
        Ok(ApiResponse::json(200, &user_json(principal)))
    }

    async fn v4_update_me(
        &self,
        req: &ApiRequest,
        principal: &Principal,
    ) -> Result<ApiResponse, ApiError> {
        // Mutating the profile is an owner/admin action — a read-only follower token
        // must not be able to change the account's unit or display name.
        principal.require(Permission::api("settings", Action::Admin))?;
        let body = req.body_json()?;
        let mut user = principal.user.clone();
        if let Some(unit) = body.get("preferredUnit").and_then(|v| v.as_str()) {
            let parsed = GlucoseUnit::parse(unit)
                .ok_or_else(|| ApiError::BadRequest(format!("unknown unit: {unit}")))?;
            user.preferred_unit = parsed.as_str().to_string();
        }
        if let Some(name) = body.get("displayName").and_then(|v| v.as_str()) {
            user.display_name = Some(name.to_string());
        }
        self.storage.upsert_user(&user).await?;
        let updated = Principal {
            user,
            scopes: super::ScopeSet::all(),
            subject: principal.subject.clone(),
        };
        Ok(ApiResponse::json(200, &user_json(&updated)))
    }

    async fn v4_list_tokens(&self, principal: &Principal) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("tokens", Action::Admin))?;
        let tokens = self.storage.list_device_tokens(&principal.user.id).await?;
        let list: Vec<Value> = tokens.iter().map(token_json).collect();
        Ok(ApiResponse::json(200, &json!({ "tokens": list })))
    }

    /// Mint a device token. The raw secret is returned **once** and never stored.
    async fn v4_create_token(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("tokens", Action::Admin))?;
        let body = req.body_json().unwrap_or_else(|_| json!({}));
        let name = body
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("device")
            .to_string();
        let scopes: Vec<String> = body
            .get("scopes")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            // Default: a read-only follower token. Callers grant create scopes explicitly.
            .unwrap_or_else(|| vec!["api:entries:read".into(), "api:treatments:read".into()]);

        // No privilege escalation: a token may only be issued scopes the caller itself
        // holds. The owner holds `*:*:*` so this is a no-op for them, but it stops a
        // token that merely has `api:tokens:admin` from minting a broader one.
        for s in &scopes {
            let scope = Scope::parse(s)
                .ok_or_else(|| ApiError::BadRequest(format!("malformed scope '{s}'")))?;
            if !principal.scopes.covers(&scope) {
                return Err(ApiError::Forbidden(format!(
                    "cannot grant scope '{s}' beyond your own access"
                )));
            }
        }

        let raw = format!("nk_{}", Uuid::new_v4().simple());
        let token = DeviceToken {
            id: Uuid::new_v4().to_string(),
            user_id: principal.user.id.clone(),
            name,
            token_hash: token_hash(&raw),
            scopes,
            created_at: now_ms,
            last_used_at: None,
            revoked: false,
            legacy_hash: Some(legacy_hash(&raw)),
        };
        self.storage.insert_device_token(&token).await?;
        let mut out = token_json(&token);
        if let Value::Object(map) = &mut out {
            // Shown exactly once — the client must store it now.
            map.insert("token".into(), json!(raw));
        }
        Ok(ApiResponse::json(201, &out))
    }

    async fn v4_revoke_token(
        &self,
        principal: &Principal,
        id: &str,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("tokens", Action::Admin))?;
        let revoked = self.storage.revoke_device_token(&principal.user.id, id).await?;
        if revoked {
            Ok(ApiResponse::empty(204))
        } else {
            Err(ApiError::NotFound)
        }
    }
}

impl<S: Storage> ApiService<S> {
    /// List the caller's connectors (no secrets).
    async fn v4_list_connectors(&self, principal: &Principal) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("connectors", Action::Admin))?;
        let creds = self.storage().list_connector_credentials(&principal.user.id).await?;
        let list: Vec<Value> = creds.iter().map(connector_json).collect();
        Ok(ApiResponse::json(200, &json!({ "connectors": list })))
    }

    /// Create/update a connector credential. The secret is encrypted at rest and
    /// never returned.
    async fn v4_put_connector(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        provider: &str,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("connectors", Action::Admin))?;
        let Some(key) = self.connector_key() else {
            return Err(ApiError::Forbidden("connectors are not enabled on this server".into()));
        };
        let body = req.body_json()?;
        let enabled = body.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);

        // Build the provider-specific secret blob, validating required fields.
        let (secret, region) = match provider {
            "dexcom" => {
                let region = body
                    .get("region")
                    .and_then(|v| v.as_str())
                    .unwrap_or("us")
                    .to_string();
                let secret = json!({
                    "username": req_field(&body, "username")?,
                    "password": req_field(&body, "password")?,
                    "region": region,
                });
                (secret, Some(region))
            }
            "librelinkup" => {
                let secret = json!({
                    "email": req_field(&body, "email")?,
                    "password": req_field(&body, "password")?,
                });
                (secret, None)
            }
            "nightscout" => {
                // Pull entries from another Nightscout/NightKnight instance. The URL is
                // user-supplied, so validate it (https, public host) before storing.
                let url = req_field(&body, "url")?;
                if !nightknight_connectors::nightscout::is_safe_base(&url) {
                    return Err(ApiError::BadRequest(
                        "nightscout url must be https to a public host".into(),
                    ));
                }
                let secret = json!({
                    "url": nightknight_connectors::nightscout::normalize_base(&url),
                    "secret": req_field(&body, "secret")?,
                });
                (secret, None)
            }
            other => return Err(ApiError::BadRequest(format!("unknown provider '{other}'"))),
        };

        let secret_enc = nightknight_crypto::encrypt_str(&key, &secret.to_string())
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        let cred = ConnectorCredential {
            user_id: principal.user.id.clone(),
            provider: provider.to_string(),
            enabled,
            secret_enc,
            region,
            created_at: now_ms,
            updated_at: now_ms,
            last_sync_at: None,
            last_status: None,
        };
        self.storage().upsert_connector_credential(&cred).await?;
        Ok(ApiResponse::json(200, &connector_json(&cred)))
    }

    async fn v4_delete_connector(
        &self,
        principal: &Principal,
        provider: &str,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("connectors", Action::Admin))?;
        if self.storage().delete_connector_credential(&principal.user.id, provider).await? {
            Ok(ApiResponse::empty(204))
        } else {
            Err(ApiError::NotFound)
        }
    }

    /// Register an APNs device token for **silent-push** background refresh, scoped to the
    /// calling user. Idempotent: iOS re-POSTs its token on every launch and whenever iOS
    /// rotates it, so a repeat registration updates the one row.
    ///
    /// Authorization is `entries:read` — deliberately the *follower* permission, not a
    /// settings/tokens admin scope. The iOS app authenticates with a read-only device
    /// token, and "register the device I'm reading on" is part of following; the write
    /// only ever touches the caller's own rows, so a broader scope would just lock the
    /// real client out. (Compare `/me`, which mutates the account and needs `settings:admin`.)
    async fn v4_push_register(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let body = req.body_json()?;
        // An APNs device token is hex. Historically 32 bytes (64 hex chars); newer tokens
        // can be longer, so accept a generous hex range and reject anything else outright —
        // a malformed token would only ever yield `BadDeviceToken` from APNs.
        let token = body
            .get("token")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|t| t.len() >= 16 && t.len() <= 256 && t.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| ApiError::BadRequest("token must be a hex APNs device token".into()))?;
        // The device reports which APNs environment it minted the token under; anything but
        // "production" is treated as sandbox (a development build's safe default).
        let environment = match body.get("environment").and_then(|v| v.as_str()) {
            Some("production") => "production",
            _ => "sandbox",
        };
        let push = PushToken {
            user_id: principal.user.id.clone(),
            token: token.to_string(),
            environment: environment.to_string(),
            bundle_id: self.push_bundle_id(),
            updated_at: now_ms,
        };
        self.storage.upsert_push_token(&push).await?;
        Ok(ApiResponse::json(200, &json!({ "ok": true })))
    }

    /// Unregister a device token (on sign-out, or when the client drops it). Scoped to the
    /// caller, so one user can never delete another's token.
    async fn v4_push_unregister(
        &self,
        req: &ApiRequest,
        principal: &Principal,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let body = req.body_json()?;
        let token = body
            .get("token")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ApiError::BadRequest("token is required".into()))?;
        if self.storage.delete_push_token(&principal.user.id, token).await? {
            Ok(ApiResponse::empty(204))
        } else {
            Err(ApiError::NotFound)
        }
    }
}

/// Safe (secret-free) view of a connector credential.
fn connector_json(c: &ConnectorCredential) -> Value {
    json!({
        "provider": c.provider,
        "enabled": c.enabled,
        "region": c.region,
        "updatedAt": c.updated_at,
        "lastSyncAt": c.last_sync_at,
        "lastStatus": c.last_status,
    })
}

/// Extract a required string field from a request body.
fn req_field(v: &Value, key: &str) -> Result<String, ApiError> {
    v.get(key)
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ApiError::BadRequest(format!("missing '{key}'")))
}

/// One day's glucose summary for the Data view (the per-day decoration). The mean / A1c
/// estimates / CV are time-weighted (count fallback), consistent with `/analytics`;
/// min/max and TIR stay count-based.
struct DayGlucose {
    mean: Option<f64>,
    min: f64,
    max: f64,
    ugmi: Option<f64>,
    gmi: Option<f64>,
    cv: Option<f64>,
    tir: TimeInRange,
    /// Expected readings for this day against its OWN cadence (see [`day_expected_per_day`]),
    /// so coverage isn't judged against one global rate.
    expected_per_day: i64,
}

impl DayGlucose {
    /// Summarise one day's readings. `readings` must be non-empty (the caller only
    /// decorates days that loaded ≥ 1 reading). `expected_per_day` is this day's own
    /// expected reading count, computed by the caller from the same readings.
    fn compute(
        readings: &[GlucoseReading],
        t: &TirThresholds,
        tw_gap: i64,
        expected_per_day: i64,
    ) -> DayGlucose {
        let s = GlucoseSummary::compute(readings, t);
        let hd = headline(&s, readings, tw_gap);
        let (min, max) = readings.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |acc, r| {
            let v = r.value.mgdl();
            (acc.0.min(v), acc.1.max(v))
        });
        DayGlucose {
            mean: hd.mean,
            min,
            max,
            ugmi: hd.ugmi,
            gmi: hd.gmi,
            cv: hd.cv,
            tir: s.tir,
            expected_per_day,
        }
    }
}

/// Result of [`ApiService::collect_day_glucose_stats`]: per-day decorations plus the
/// metadata `/days` reports about the (batched) stats window.
struct DayGlucoseStats {
    /// Per-day glucose summary, keyed by local day-number. Only days fully loaded within
    /// the batch budget appear; the rest are count-only.
    by_day: std::collections::HashMap<i64, DayGlucose>,
    /// The most recent batch's readings, for the "recent window" headline + cadence.
    recent_readings: Vec<GlucoseReading>,
    /// Total readings loaded across all batches (the `statsWindowReadings` figure).
    loaded: i64,
    /// True if older days were left count-only because the batch budget was reached.
    capped: bool,
    /// Sampling cadence inferred from the most recent batch.
    cadence_ms: i64,
}

/// Build a [`GlucoseReading`] from a stored entry document, if it carries a value.
fn reading_from_doc(d: &StoredDoc) -> Option<GlucoseReading> {
    let entry: Entry = serde_json::from_value(d.doc.clone()).ok()?;
    let value = entry.glucose_value().ok().flatten()?;
    Some(GlucoseReading::new(d.mills, value))
}

/// The sensor's first-party trend arrow stored on an entry, if it carries a recognised
/// `direction` (Dexcom Share / LibreLinkUp persist one; manual entries usually don't).
fn direction_from_doc(d: &StoredDoc) -> Option<Direction> {
    let entry: Entry = serde_json::from_value(d.doc.clone()).ok()?;
    entry.direction_parsed()
}

/// Expected readings for **one local day**, measured against that day's *own* sampling
/// cadence rather than a single global rate. This is what stops a genuinely complete day
/// from a slower-sensor era (e.g. 5-min Dexcom, n≈288) from being mislabelled
/// under-covered against a faster recent cadence (e.g. 1-min LibreLinkUp, ≈1440/day).
///
/// When the day's full readings are loaded we take their robust within-day median gap
/// (via [`infer_cadence_ms`], whose ~2 h gap filter ignores mid-day outages so a real
/// sensor dropout still shows as *reduced* coverage). For count-only days outside the
/// stats window we estimate the cadence from the day's own span, `(last-first)/(n-1)`; a
/// lone reading has no interval to measure and falls back to the global cadence.
fn day_expected_per_day(
    d: &DayCount,
    day_readings: Option<&Vec<GlucoseReading>>,
    fallback_cadence_ms: i64,
    tz: i64,
) -> i64 {
    let cadence_ms = match day_readings {
        // Full day loaded: robust median of within-day gaps.
        Some(rs) if rs.len() as i64 == d.n && rs.len() >= 2 => infer_cadence_ms(rs, tz),
        // Count-only (or partially loaded) day: estimate from the day's span. Clamped to
        // the same sane [1 min, 1 h] range `infer_cadence_ms` uses.
        _ if d.n >= 2 && d.last_ms > d.first_ms => {
            ((d.last_ms - d.first_ms) / (d.n - 1)).clamp(60_000, 60 * 60_000)
        }
        _ => fallback_cadence_ms,
    };
    (timeutil::DAY_MS as f64 / cadence_ms as f64).round() as i64
}

/// A `Content-Disposition` value that prompts a download under `name`. `name` is built
/// from our own `filename_stem` (ASCII letters, digits, `-` and `_`), so it needs no
/// escaping and carries no user-controlled text.
fn attachment(name: &str) -> String {
    format!("attachment; filename=\"{name}\"")
}

/// The whole-minute downsample bucket for an aggregated export over a `window_ms`-long
/// range: the smallest bucket that keeps `window / bucket ≤ EXPORT_TARGET_SAMPLES`,
/// floored at `MIN_EXPORT_BUCKET_MS` (one minute). Widening the bucket only as far as the
/// window demands means short/normal-cadence exports aren't thinned at all (a 14-day
/// window stays at 1-minute buckets), while a 90-day window settles on a few-minute bucket
/// that still gives thousands of points per AGP bin.
fn downsample_bucket_ms(window_ms: i64) -> i64 {
    let window_ms = window_ms.max(1);
    // ceil(window / target) = minimum ms-per-sample to land at/under the target count.
    let per_sample = (window_ms + EXPORT_TARGET_SAMPLES - 1) / EXPORT_TARGET_SAMPLES;
    // Round that up to a whole minute, floored at one minute.
    let minutes = ((per_sample + MIN_EXPORT_BUCKET_MS - 1) / MIN_EXPORT_BUCKET_MS).max(1);
    minutes * MIN_EXPORT_BUCKET_MS
}

/// The caller's UTC offset in minutes (east of UTC), for localising time-of-day
/// analytics. Defaults to 0 (UTC) and is clamped to the real-world ±14h range.
fn tz_offset(req: &ApiRequest) -> i64 {
    req.query_int("tzOffset").unwrap_or(0).clamp(-14 * 60, 14 * 60)
}

/// Resolve a `?hours=`/`?count=` window into `(since_ms, limit)`.
fn window(req: &ApiRequest, now_ms: i64) -> (i64, i64) {
    let hours = req.query_int("hours").unwrap_or(DEFAULT_HOURS).clamp(1, 24 * 90);
    let since = now_ms - hours * 3_600_000;
    let limit = req.query_int("count").unwrap_or(MAX_ANALYTICS_POINTS).clamp(1, MAX_ANALYTICS_POINTS);
    (since, limit)
}

fn user_json(principal: &Principal) -> Value {
    json!({
        "id": principal.user.id,
        "subject": principal.user.subject,
        "displayName": principal.user.display_name,
        "preferredUnit": principal.user.preferred_unit,
        "isAdmin": principal.user.is_admin,
    })
}

fn token_json(t: &DeviceToken) -> Value {
    json!({
        "id": t.id,
        "name": t.name,
        "scopes": t.scopes,
        "createdAt": t.created_at,
        "lastUsedAt": t.last_used_at,
        "revoked": t.revoked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nightknight_core::units::GlucoseValue;

    /// Midnight UTC on a fixed, stable day so day-number bucketing is deterministic.
    const BASE: i64 = 19_500 * timeutil::DAY_MS;

    fn reading(ms: i64) -> GlucoseReading {
        GlucoseReading::new(ms, GlucoseValue::from_mgdl(120.0).unwrap())
    }

    fn day_count(day_index: i64, n: i64, first_ms: i64, last_ms: i64) -> DayCount {
        DayCount { day_index, n, first_ms, last_ms }
    }

    /// A complete day whose readings are fully loaded is judged against ITS OWN cadence
    /// (here 5-min → ~288), not the faster global one passed as the fallback.
    #[test]
    fn loaded_day_uses_its_own_median_cadence() {
        let readings: Vec<GlucoseReading> =
            (0..288).map(|i| reading(BASE + i * 5 * 60_000)).collect();
        let d = day_count(19_500, 288, BASE, BASE + 287 * 5 * 60_000);
        // Global fallback is a fast 1-minute cadence (≈1440/day); the per-day value must
        // ignore it and reflect this day's real 5-minute cadence.
        let expected = day_expected_per_day(&d, Some(&readings), 60_000, 0);
        assert_eq!(expected, 288, "5-min day expects ~288, not the 1-min global 1440");
    }

    /// A real mid-day outage must REDUCE coverage, not be absorbed into a slower inferred
    /// cadence: the >2 h gap is excluded when inferring cadence, so the expectation stays
    /// at the full-day 288 and a half-empty day reads as under-covered.
    #[test]
    fn loaded_day_outage_reduces_coverage_not_cadence() {
        // 12 h of 5-min data, a 6 h outage, then a short resume.
        let mut readings: Vec<GlucoseReading> =
            (0..144).map(|i| reading(BASE + i * 5 * 60_000)).collect();
        let resume = BASE + 18 * 3_600_000;
        readings.extend((0..36).map(|i| reading(resume + i * 5 * 60_000)));
        let n = readings.len() as i64;
        let d = day_count(19_500, n, readings[0].date_ms, readings[readings.len() - 1].date_ms);
        let expected = day_expected_per_day(&d, Some(&readings), 60_000, 0);
        assert_eq!(expected, 288, "the outage must not inflate the inferred cadence");
        assert!((n as f64 / expected as f64) < 0.75, "a half-empty day reads as under-covered");
    }

    /// Count-only days (outside the loaded stats window) estimate cadence from their own
    /// span, so an old 5-min day still expects ~288 even when the global cadence is faster.
    #[test]
    fn count_only_day_estimates_cadence_from_span() {
        let d = day_count(10_000, 288, BASE, BASE + 287 * 5 * 60_000);
        let expected = day_expected_per_day(&d, None, 60_000, 0);
        assert_eq!(expected, 288, "span-based estimate recovers the 5-min cadence");
    }

    /// A lone reading has no interval to measure and falls back to the global cadence.
    #[test]
    fn single_reading_day_falls_back_to_global_cadence() {
        let d = day_count(10_001, 1, BASE, BASE);
        // Global 10-min cadence → 144 expected.
        let expected = day_expected_per_day(&d, None, 10 * 60_000, 0);
        assert_eq!(expected, 144);
    }

    /// The aggregated-export bucket widens only as far as a long window needs, and never
    /// below one minute — so normal-length exports keep every reading while a 90-day
    /// report stays bounded near `EXPORT_TARGET_SAMPLES`.
    #[test]
    fn downsample_bucket_scales_with_window_but_floors_at_one_minute() {
        let day = timeutil::DAY_MS;
        // Short/normal windows: 1-minute buckets (no thinning of 1–5-min CGM data).
        assert_eq!(downsample_bucket_ms(day), 60_000, "a 1-day window stays at 1-min buckets");
        assert_eq!(downsample_bucket_ms(14 * day), 60_000, "the default 14-day window is un-thinned");
        assert_eq!(downsample_bucket_ms(1), 60_000, "a degenerate tiny window floors at 1 min");
        // A 90-day window widens the bucket, but the resulting sample count stays bounded.
        let b90 = downsample_bucket_ms(90 * day);
        assert!(b90 > 60_000, "a 90-day window needs a wider bucket, got {b90}");
        assert_eq!(b90 % 60_000, 0, "buckets are whole minutes");
        assert!(90 * day / b90 <= EXPORT_TARGET_SAMPLES, "sample count is held under the target");
        // And it's the SMALLEST such whole-minute bucket (one minute narrower would overshoot).
        assert!(90 * day / (b90 - 60_000) > EXPORT_TARGET_SAMPLES, "bucket is the minimal width needed");
    }
}

#[cfg(test)]
mod days_stats_tests {
    //! Unit tests for the batched per-day stats walk that backs `/api/v4/days`. They drive
    //! [`ApiService::collect_day_glucose_stats`] directly with a small page size so the
    //! batching/coverage behaviour is exercised without ingesting tens of thousands of rows.

    use crate::ApiService;
    use nightknight_storage::{Collection, Storage};
    use nightknight_store_sql::SqlStore;
    use serde_json::json;

    const NOW: i64 = 1_700_000_000_000;
    const DAY_MS: i64 = 86_400_000;

    /// Build a service with `values.len()` consecutive local (UTC) days of data, 3 readings
    /// each; `values[k]` is the constant glucose for day `k` (0 = newest). Returns the
    /// service and the stored (namespaced) user id.
    async fn svc_with_days(values: &[i64]) -> (ApiService<SqlStore>, String) {
        let store = SqlStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        let svc = ApiService::new(store);
        let mut entries = Vec::new();
        for (k, &v) in values.iter().enumerate() {
            for j in 0..3i64 {
                let date = NOW - (k as i64) * DAY_MS - j * 60_000;
                entries.push(json!({ "type": "sgv", "date": date, "sgv": v, "device": "t" }));
            }
        }
        svc.ingest_entries("alice@cooney.be", entries, NOW).await.unwrap();
        // ingest keys the user in the service namespace (see `tenant_subject`).
        let uid = svc
            .storage()
            .get_user_by_subject("service:alice@cooney.be")
            .await
            .unwrap()
            .unwrap()
            .id;
        (svc, uid)
    }

    async fn counts(svc: &ApiService<SqlStore>, uid: &str) -> Vec<nightknight_storage::DayCount> {
        svc.storage().daily_counts(Collection::Entries, uid, "sgv", 0).await.unwrap()
    }

    /// The headline symptom of issue #17: with dense data, a flat reading cap left older
    /// days with no average. The batched walk decorates EVERY day, not just the newest —
    /// here with a page size that forces one day per batch.
    #[tokio::test]
    async fn decorates_every_day_across_multiple_batches() {
        let (svc, uid) = svc_with_days(&[120, 90, 200, 60]).await;
        let counts = counts(&svc, &uid).await;
        assert_eq!(counts.len(), 4, "four days have data");

        let stats = svc.collect_day_glucose_stats(&uid, &counts, 0, 4, 10).await.unwrap();
        assert_eq!(stats.by_day.len(), 4, "all four days get a per-day summary, not just the newest");
        assert!(!stats.capped, "the batch budget was not exhausted");
        assert_eq!(stats.loaded, 12, "every reading was loaded across the batches");
        // Each day's mean reflects its OWN readings, not a blended window.
        assert_eq!(stats.by_day.get(&counts[0].day_index).unwrap().mean.unwrap().round(), 120.0);
        assert_eq!(stats.by_day.get(&counts[3].day_index).unwrap().mean.unwrap().round(), 60.0);
    }

    /// A batch spans several days when their combined readings fit one page — and every day
    /// it covers is decorated (no day is split across the date-range query boundary).
    #[tokio::test]
    async fn batches_span_multiple_days_when_they_fit() {
        let (svc, uid) = svc_with_days(&[120, 90, 200, 60]).await;
        let counts = counts(&svc, &uid).await;
        // page_size 8 fits two days (6 readings) per batch → 2 batches cover all 4 days.
        let stats = svc.collect_day_glucose_stats(&uid, &counts, 0, 8, 10).await.unwrap();
        assert_eq!(stats.by_day.len(), 4, "a multi-day batch decorates every day it covers");
        assert!(!stats.capped);
    }

    /// When the batch budget is exhausted, older days fall back to count-only and the result
    /// is flagged `capped` — the honest "stats cover the recent window" behaviour.
    #[tokio::test]
    async fn budget_caps_older_days_to_count_only() {
        let (svc, uid) = svc_with_days(&[120, 90, 200, 60]).await;
        let counts = counts(&svc, &uid).await;
        // 2 batches allowed, 1 day each → 2 newest days decorated, 2 oldest count-only.
        let stats = svc.collect_day_glucose_stats(&uid, &counts, 0, 4, 2).await.unwrap();
        assert_eq!(stats.by_day.len(), 2, "only days within the batch budget are decorated");
        assert!(stats.capped, "remaining older days are reported as capped");
        assert!(stats.by_day.contains_key(&counts[0].day_index), "newest day decorated");
        assert!(!stats.by_day.contains_key(&counts[3].day_index), "oldest day is count-only");
    }
}
