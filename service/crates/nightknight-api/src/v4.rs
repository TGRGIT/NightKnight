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
use nightknight_core::analytics::{
    self, Coverage, GlucoseEpisode, GlucoseReading, GlucoseSummary, GlycemiaRiskIndex, PeriodStats,
    TimeInRange, TirThresholds, DEFAULT_EPISODE_GAP_MS, DEFAULT_MAX_GAP_MS,
};
use nightknight_core::documents::Entry;
use nightknight_core::import::{parse_glucose_csv, DateOrder};
use nightknight_core::timeutil;
use nightknight_core::trend::{self, Direction};
use nightknight_core::units::GlucoseUnit;
use nightknight_storage::{
    Collection, ConnectorCredential, DayCount, DeviceToken, DocQuery, Storage, StoredDoc,
};

use super::{ApiError, ApiRequest, ApiResponse, ApiService, EdgeIdentity, Principal};
use crate::hashing::{legacy_hash, token_hash};
use crate::http::Method;
use crate::{SERVICE_NAME, SERVICE_VERSION};

const DEFAULT_HOURS: i64 = 24;
const MAX_ANALYTICS_POINTS: i64 = 20_000;
/// How many recent readings `current` pulls to estimate a fallback trend — enough to
/// span the 15-minute regression window at 5-minute cadence with headroom.
const CURRENT_TREND_POINTS: i64 = 8;
/// If the latest reading is older than this, the trend is stale and we report no arrow
/// rather than a misleading one. The research staleness basis is ~2× the source's
/// cadence (≈10–11 min at the 5-minute Dexcom cadence); we allow a small grace for one
/// late sample. The reading *value* itself is always still returned. (Distinct from the
/// 15-minute regression window — this guards the latest sample's freshness, not span.)
const TREND_STALE_MS: i64 = 12 * 60_000;
/// Tolerance for matching a reading's lagged partner in CONGA / MODD — generous enough
/// to find a near-match across 5-min and 15-min cadences without matching a far-off time.
const LAG_TOLERANCE_MS: i64 = 10 * 60_000;
/// CONGA lag (hours) surfaced in the advanced-variability block.
const CONGA_HOURS: f64 = 2.0;
/// How many of the most recent episodes the analytics payload lists for the UI feed.
const RECENT_EPISODES: usize = 8;
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
    async fn v4_analytics(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let hours = req.query_int("hours").unwrap_or(DEFAULT_HOURS).clamp(1, 24 * 90);
        let tz = tz_offset(req);
        let window_ms = hours * 3_600_000;
        let since = now_ms - window_ms;
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
        let summary = GlucoseSummary::compute(&readings, &t);

        // Cadence-aware gap handling: infer the device's sampling rate and scale coverage,
        // episode breaks and time-weighting to it rather than assuming 5-minute CGM (so a
        // perfect 15-minute Libre isn't mislabelled "limited", and a sparse source can
        // still form episodes). Floors keep normal 1–5-min data byte-for-byte unchanged.
        let cadence_ms = infer_cadence_ms(&readings, tz);
        let (tw_gap, episode_gap) = gap_caps(cadence_ms);

        // Headline mean / SD / CV / A1c estimates are time-weighted so non-uniform
        // sampling (bursts, mixed cadence) can't bias the average.
        let h = headline(&summary, &readings, tw_gap);

        // Data sufficiency, GRI, time-weighted TIR, and advanced variability.
        let coverage = Coverage::compute(&readings, window_ms, cadence_ms, tz);
        // GRI 0 means "perfect glycemia", so an empty window must report null — not a
        // fabricated best-possible score — like every other metric here.
        let gri = (summary.n > 0).then(|| GlycemiaRiskIndex::from_tir(&summary.tir));
        let weighted = TimeInRange::compute_weighted(&readings, &t, tw_gap);
        let mage = analytics::mage(&readings);
        let conga = analytics::conga(&readings, CONGA_HOURS, LAG_TOLERANCE_MS);
        let modd = analytics::modd(&readings, LAG_TOLERANCE_MS);
        let patterns = analytics::time_of_day_patterns(&readings, &t, tz);

        // Episodes: events/day are normalised over the days that actually carry data.
        let days = coverage.distinct_days.max(1) as f64;
        let lows = analytics::detect_episodes(&readings, t.low, true, tz, episode_gap);
        let very_lows = analytics::detect_episodes(&readings, t.very_low, true, tz, episode_gap);
        let highs = analytics::detect_episodes(&readings, t.high, false, tz, episode_gap);
        let very_highs = analytics::detect_episodes(&readings, t.very_high, false, tz, episode_gap);

        Ok(ApiResponse::json(
            200,
            &json!({
                "hours": hours,
                "tzOffset": tz,
                "cadenceMs": cadence_ms,
                "n": summary.n,
                "meanMgdl": h.mean,
                "sdMgdl": h.sd,
                "uGmiPercent": h.ugmi,
                "gmiPercent": h.gmi,
                "estimatedA1cPercent": h.ea1c,
                "cvPercent": h.cv,
                "coverage": coverage_json(&coverage),
                "timeInRange": tir_json(&summary.tir),
                "timeInRangeWeighted": weighted.as_ref().map(tir_json),
                "gri": {
                    "value": gri.map(|g| g.gri),
                    "zone": gri.map(|g| g.zone.label()),
                    "hypoComponent": gri.map(|g| g.hypo_component),
                    "hyperComponent": gri.map(|g| g.hyper_component),
                },
                "variability": {
                    "jIndex": h.j_index,
                    "mage": mage,
                    "congaHours": CONGA_HOURS,
                    "conga": conga,
                    "modd": modd,
                },
                "patterns": patterns.iter().map(period_json).collect::<Vec<_>>(),
                "episodes": {
                    "low": episode_summary_json(&lows, days),
                    "veryLow": episode_summary_json(&very_lows, days),
                    "high": episode_summary_json(&highs, days),
                    "veryHigh": episode_summary_json(&very_highs, days),
                    "recent": recent_episodes_json(&lows, &highs),
                },
            }),
        ))
    }

    /// Per-day data coverage + glucose stats for the **Data** view — the answer to "did
    /// my history actually import, and what does each day look like?".
    ///
    /// Two tiers, by design, so it scales to thousands of days:
    /// * **Every** local day that has ≥1 sgv reading is listed with its reading `count`
    ///   and first/last reading time, from the cheap indexed [`Storage::daily_counts`]
    ///   aggregation (no document bodies loaded).
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
                let mut obj = json!({
                    "date": timeutil::date_string_from_day_number(d.day_index),
                    "dayIndex": d.day_index,
                    "n": d.n,
                    "firstMs": d.first_ms,
                    "lastMs": d.last_ms,
                });
                // Attach the per-day glucose summary for every day the batched walk fully
                // loaded. Days beyond the batch budget (oldest history) carry the count
                // only — the UI shows that honestly via `statsCapped`.
                if let (Some(s), Value::Object(m)) = (stats.by_day.get(&d.day_index), &mut obj) {
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
                        by_day.insert(d.day_index, DayGlucose::compute(rs, &t, tw_gap));
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
        let bins: Vec<Value> = analytics::agp_bins(&readings, bin, tz)
            .iter()
            .map(|b| {
                json!({
                    "minuteOfDay": b.minute_of_day,
                    "n": b.n,
                    "p05": b.p05,
                    "p25": b.p25,
                    "p50": b.p50,
                    "p75": b.p75,
                    "p95": b.p95,
                })
            })
            .collect();
        Ok(ApiResponse::json(
            200,
            &json!({ "days": days, "binMinutes": bin, "tzOffset": tz, "n": readings.len(), "bins": bins }),
        ))
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
}

impl DayGlucose {
    /// Summarise one day's readings. `readings` must be non-empty (the caller only
    /// decorates days that loaded ≥ 1 reading).
    fn compute(readings: &[GlucoseReading], t: &TirThresholds, tw_gap: i64) -> DayGlucose {
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

/// Infer the CGM sampling cadence (ms) from a window of readings — the **median** gap
/// between consecutive *same-day* readings, clamped to a sane [1 min, 1 h] range. This is
/// what scales coverage %, episode breaks and time-weighting to the actual device (5-min
/// Dexcom, 1-min LibreLinkUp, 15-min Libre, hourly) instead of assuming 5-minute CGM.
/// The median is robust to occasional gaps; collecting only within-day gaps up to ~2 h
/// keeps overnight breaks and sensor changes from skewing it, while still admitting a
/// genuinely hourly device. Defaults to the 5-minute standard when there isn't enough
/// data to tell.
fn infer_cadence_ms(readings: &[GlucoseReading], tz: i64) -> i64 {
    let mut times: Vec<i64> = readings.iter().map(|r| r.date_ms).collect();
    times.sort_unstable();
    let mut gaps: Vec<i64> = times
        .windows(2)
        .map(|w| (w[0], w[1] - w[0]))
        .filter(|&(t0, gap)| {
            gap > 0
                && gap <= 2 * 3_600_000
                && timeutil::day_number(t0, tz) == timeutil::day_number(t0 + gap, tz)
        })
        .map(|(_, gap)| gap)
        .collect();
    if gaps.is_empty() {
        return 5 * 60_000;
    }
    gaps.sort_unstable();
    gaps[gaps.len() / 2].clamp(60_000, 60 * 60_000)
}

/// The headline scalar metrics (mean, SD, CV and the A1c estimates), computed
/// **time-weighted** so dense bursts / non-uniform sampling don't bias the average,
/// falling back to the count-based `summary` when there aren't enough valid intervals to
/// time-weight (e.g. a single reading). For clean uniform CGM the two agree to rounding,
/// so ordinary users see no change; only skewed sampling is corrected.
struct Headline {
    mean: Option<f64>,
    sd: Option<f64>,
    cv: Option<f64>,
    ugmi: Option<f64>,
    gmi: Option<f64>,
    ea1c: Option<f64>,
    j_index: Option<f64>,
}

fn headline(summary: &GlucoseSummary, readings: &[GlucoseReading], max_gap_ms: i64) -> Headline {
    let tw = analytics::time_weighted_stats(readings, max_gap_ms);
    let mean = tw.map(|s| s.mean_mgdl).or(summary.mean_mgdl);
    let sd = tw.map(|s| s.sd_mgdl).or(summary.sd_mgdl);
    let cv = match (mean, sd) {
        (Some(m), Some(s)) if m != 0.0 => Some(s / m * 100.0),
        _ => None,
    };
    Headline {
        mean,
        sd,
        cv,
        ugmi: mean.map(analytics::updated_gmi_percent),
        gmi: mean.map(analytics::gmi_percent),
        ea1c: mean.map(analytics::estimated_a1c_percent),
        j_index: analytics::j_index(mean, sd),
    }
}

/// Gap caps derived from the inferred sampling cadence, so coverage, episode breaks and
/// time-weighting all scale with the actual device rather than assuming 5-minute CGM.
/// Each keeps its consensus floor (so normal 1–5-min data is unchanged) but widens for
/// sparse sources (e.g. hourly readings) where the fixed floor would wrongly discard data
/// or miss every episode. `2× cadence` is the consensus "discontinuity" guidance.
fn gap_caps(cadence_ms: i64) -> (i64, i64) {
    let tw_gap = (2 * cadence_ms).max(DEFAULT_MAX_GAP_MS);
    let episode_gap = (2 * cadence_ms).max(DEFAULT_EPISODE_GAP_MS);
    (tw_gap, episode_gap)
}

/// The caller's UTC offset in minutes (east of UTC), for localising time-of-day
/// analytics. Defaults to 0 (UTC) and is clamped to the real-world ±14h range.
fn tz_offset(req: &ApiRequest) -> i64 {
    req.query_int("tzOffset").unwrap_or(0).clamp(-14 * 60, 14 * 60)
}

/// Serialise a Time-in-Range distribution (shared by count- and time-weighted TIR).
fn tir_json(tir: &nightknight_core::analytics::TimeInRange) -> Value {
    json!({
        "veryLowPct": tir.very_low_pct,
        "lowPct": tir.low_pct,
        "inRangePct": tir.in_range_pct,
        "highPct": tir.high_pct,
        "veryHighPct": tir.very_high_pct,
    })
}

/// Serialise data-sufficiency coverage.
fn coverage_json(c: &Coverage) -> Value {
    json!({
        "n": c.n,
        "firstReading": c.first_ms,
        "lastReading": c.last_ms,
        "daysCovered": c.days_covered,
        "distinctDays": c.distinct_days,
        "percentActive": c.percent_active,
        "sufficient": c.sufficient,
    })
}

/// Serialise one time-of-day period's summary.
fn period_json(p: &PeriodStats) -> Value {
    json!({
        "startHour": p.start_hour,
        "endHour": p.end_hour,
        "n": p.summary.n,
        "meanMgdl": p.summary.mean_mgdl,
        "inRangePct": p.summary.tir.in_range_pct,
        "cvPercent": p.summary.cv_percent,
    })
}

/// Serialise the roll-up of a set of episodes for one threshold.
fn episode_summary_json(episodes: &[GlucoseEpisode], days: f64) -> Value {
    let s = analytics::EpisodeSummary::of(episodes, days);
    json!({
        "count": s.count,
        "nocturnal": s.nocturnal_count,
        "perDay": s.per_day,
        "longestMin": s.longest_min,
        "totalMin": s.total_min,
    })
}

/// The most recent episodes (lows + highs interleaved, newest first) for the UI feed.
fn recent_episodes_json(lows: &[GlucoseEpisode], highs: &[GlucoseEpisode]) -> Vec<Value> {
    let mut all: Vec<(&str, &GlucoseEpisode)> = lows
        .iter()
        .map(|e| ("low", e))
        .chain(highs.iter().map(|e| ("high", e)))
        .collect();
    all.sort_by(|a, b| b.1.start_ms.cmp(&a.1.start_ms));
    all.into_iter()
        .take(RECENT_EPISODES)
        .map(|(kind, e)| {
            json!({
                "kind": kind,
                "start": e.start_ms,
                "end": e.end_ms,
                "durationMin": e.duration_min,
                "extremeMgdl": e.extreme_mgdl,
                "nocturnal": e.nocturnal,
            })
        })
        .collect()
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
