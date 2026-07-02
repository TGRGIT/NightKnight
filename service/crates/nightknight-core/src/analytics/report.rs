//! The wire-level analytics / AGP reports — the exact JSON payloads NightKnight's v4
//! API serves to first-party clients.
//!
//! This is the **single source of truth** for the report shape: the server
//! (`nightknight-api::v4`) and the iOS on-device FFI (`nightknight-ffi`) both call
//! [`analytics_value`] / [`agp_value`], so the two can never drift — an iOS build
//! computing analytics locally from raw readings decodes byte-identical JSON to what
//! the server emits. The metric maths lives in the parent [`analytics`](crate::analytics)
//! module; this module owns only the composition (which metrics, over which gaps, under
//! which field names).

use serde_json::{json, Value};

use crate::analytics::{
    self, Coverage, GlucoseEpisode, GlucoseReading, GlucoseSummary, GlycemiaRiskIndex,
    PeriodStats, TimeInRange, TirThresholds, DEFAULT_EPISODE_GAP_MS, DEFAULT_MAX_GAP_MS,
};
use crate::timeutil;

/// Tolerance for matching a reading's lagged partner in CONGA / MODD — generous enough
/// to find a near-match across 5-min and 15-min cadences without matching a far-off time.
pub const LAG_TOLERANCE_MS: i64 = 10 * 60_000;
/// CONGA lag (hours) surfaced in the advanced-variability block.
pub const CONGA_HOURS: f64 = 2.0;
/// How many of the most recent episodes the analytics payload lists for the UI feed.
pub const RECENT_EPISODES: usize = 8;

/// The full Statistical-Analysis payload over a window of readings (the body of
/// `GET /api/v4/analytics`). `readings` must already be restricted to the window —
/// the caller queried the last `hours` hours; `hours` is echoed and sets the coverage
/// denominator. `tz` is minutes east of UTC. Inputs are clamped to the same ranges the
/// server enforces so an out-of-range FFI caller can't skew the window arithmetic.
pub fn analytics_value(
    readings: &[GlucoseReading],
    hours: i64,
    tz: i64,
    t: &TirThresholds,
) -> Value {
    let hours = hours.clamp(1, 24 * 90);
    let tz = tz.clamp(-14 * 60, 14 * 60);
    let window_ms = hours * 3_600_000;
    let summary = GlucoseSummary::compute(readings, t);

    // Cadence-aware gap handling: infer the device's sampling rate and scale coverage,
    // episode breaks and time-weighting to it rather than assuming 5-minute CGM (so a
    // perfect 15-minute Libre isn't mislabelled "limited", and a sparse source can
    // still form episodes). Floors keep normal 1–5-min data byte-for-byte unchanged.
    let cadence_ms = infer_cadence_ms(readings, tz);
    let (tw_gap, episode_gap) = gap_caps(cadence_ms);

    // Headline mean / SD / CV / A1c estimates are time-weighted so non-uniform
    // sampling (bursts, mixed cadence) can't bias the average.
    let h = headline(&summary, readings, tw_gap);

    // Data sufficiency, GRI, time-weighted TIR, and advanced variability.
    let coverage = Coverage::compute(readings, window_ms, cadence_ms, tz);
    // GRI 0 means "perfect glycemia", so an empty window must report null — not a
    // fabricated best-possible score — like every other metric here.
    let gri = (summary.n > 0).then(|| GlycemiaRiskIndex::from_tir(&summary.tir));
    let weighted = TimeInRange::compute_weighted(readings, t, tw_gap);
    let mage = analytics::mage(readings);
    let conga = analytics::conga(readings, CONGA_HOURS, LAG_TOLERANCE_MS);
    let modd = analytics::modd(readings, LAG_TOLERANCE_MS);
    let patterns = analytics::time_of_day_patterns(readings, t, tz);

    // Episodes: events/day are normalised over the days that actually carry data.
    let days = coverage.distinct_days.max(1) as f64;
    let lows = analytics::detect_episodes(readings, t.low, true, tz, episode_gap);
    let very_lows = analytics::detect_episodes(readings, t.very_low, true, tz, episode_gap);
    let highs = analytics::detect_episodes(readings, t.high, false, tz, episode_gap);
    let very_highs = analytics::detect_episodes(readings, t.very_high, false, tz, episode_gap);

    json!({
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
    })
}

/// The Ambulatory Glucose Profile payload (the body of `GET /api/v4/agp`): the
/// 5/25/50/75/95 percentile bands of glucose by time of day, every day in the window
/// overlaid onto one 24-hour axis. `readings` must already be restricted to the last
/// `days` days; `bin` is the bin width in minutes; `tz` is minutes east of UTC.
pub fn agp_value(readings: &[GlucoseReading], days: i64, bin: i64, tz: i64) -> Value {
    let days = days.clamp(1, 90);
    let bin = bin.clamp(5, 60);
    let tz = tz.clamp(-14 * 60, 14 * 60);
    let bins: Vec<Value> = analytics::agp_bins(readings, bin, tz)
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
    json!({ "days": days, "binMinutes": bin, "tzOffset": tz, "n": readings.len(), "bins": bins })
}

/// Infer the CGM sampling cadence (ms) from a window of readings — the **median** gap
/// between consecutive *same-day* readings, clamped to a sane [1 min, 1 h] range. This is
/// what scales coverage %, episode breaks and time-weighting to the actual device (5-min
/// Dexcom, 1-min LibreLinkUp, 15-min Libre, hourly) instead of assuming 5-minute CGM.
/// The median is robust to occasional gaps; collecting only within-day gaps up to ~2 h
/// keeps overnight breaks and sensor changes from skewing it, while still admitting a
/// genuinely hourly device. Defaults to the 5-minute standard when there isn't enough
/// data to tell.
pub fn infer_cadence_ms(readings: &[GlucoseReading], tz: i64) -> i64 {
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

/// Gap caps derived from the inferred sampling cadence, so coverage, episode breaks and
/// time-weighting all scale with the actual device rather than assuming 5-minute CGM.
/// Each keeps its consensus floor (so normal 1–5-min data is unchanged) but widens for
/// sparse sources (e.g. hourly readings) where the fixed floor would wrongly discard data
/// or miss every episode. `2× cadence` is the consensus "discontinuity" guidance.
pub fn gap_caps(cadence_ms: i64) -> (i64, i64) {
    let tw_gap = (2 * cadence_ms).max(DEFAULT_MAX_GAP_MS);
    let episode_gap = (2 * cadence_ms).max(DEFAULT_EPISODE_GAP_MS);
    (tw_gap, episode_gap)
}

/// The headline scalar metrics (mean, SD, CV and the A1c estimates), computed
/// **time-weighted** so dense bursts / non-uniform sampling don't bias the average,
/// falling back to the count-based `summary` when there aren't enough valid intervals to
/// time-weight (e.g. a single reading). For clean uniform CGM the two agree to rounding,
/// so ordinary users see no change; only skewed sampling is corrected.
pub struct Headline {
    pub mean: Option<f64>,
    pub sd: Option<f64>,
    pub cv: Option<f64>,
    pub ugmi: Option<f64>,
    pub gmi: Option<f64>,
    pub ea1c: Option<f64>,
    pub j_index: Option<f64>,
}

pub fn headline(
    summary: &GlucoseSummary,
    readings: &[GlucoseReading],
    max_gap_ms: i64,
) -> Headline {
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

/// Serialise a Time-in-Range distribution (shared by count- and time-weighted TIR).
pub fn tir_json(tir: &TimeInRange) -> Value {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::GlucoseValue;

    fn at(ms: i64, mgdl: f64) -> GlucoseReading {
        GlucoseReading::new(ms, GlucoseValue::from_mgdl(mgdl).unwrap())
    }

    /// Every field the v4 wire contract promises is present, with the right names —
    /// the iOS DTO decoders and the FFI golden test both hang off these exact keys.
    #[test]
    fn analytics_payload_carries_the_full_v4_shape() {
        let readings: Vec<GlucoseReading> =
            (0..288).map(|i| at(i * 300_000, 100.0 + (i % 40) as f64)).collect();
        let v = analytics_value(&readings, 24, 60, &TirThresholds::default());
        for key in [
            "hours", "tzOffset", "cadenceMs", "n", "meanMgdl", "sdMgdl", "uGmiPercent",
            "gmiPercent", "estimatedA1cPercent", "cvPercent", "coverage", "timeInRange",
            "timeInRangeWeighted", "gri", "variability", "patterns", "episodes",
        ] {
            assert!(v.get(key).is_some(), "missing key {key}");
        }
        assert_eq!(v["hours"], 24);
        assert_eq!(v["tzOffset"], 60);
        assert_eq!(v["n"], 288);
        assert_eq!(v["patterns"].as_array().unwrap().len(), 4);
        for key in ["low", "veryLow", "high", "veryHigh", "recent"] {
            assert!(v["episodes"].get(key).is_some(), "missing episodes.{key}");
        }
    }

    #[test]
    fn agp_payload_carries_the_full_v4_shape() {
        let readings: Vec<GlucoseReading> =
            (0..288).map(|i| at(i * 300_000, 120.0)).collect();
        let v = agp_value(&readings, 14, 15, 0);
        assert_eq!(v["days"], 14);
        assert_eq!(v["binMinutes"], 15);
        assert_eq!(v["n"], 288);
        let bins = v["bins"].as_array().unwrap();
        assert_eq!(bins.len(), 96);
        for key in ["minuteOfDay", "n", "p05", "p25", "p50", "p75", "p95"] {
            assert!(bins[0].get(key).is_some(), "missing bin key {key}");
        }
    }

    /// Out-of-range window parameters are clamped exactly like the server clamps its
    /// query params, so an FFI caller can't skew the coverage denominator.
    #[test]
    fn inputs_are_clamped_to_server_ranges() {
        let v = analytics_value(&[], 0, 20_000, &TirThresholds::default());
        assert_eq!(v["hours"], 1);
        assert_eq!(v["tzOffset"], 14 * 60);
        let a = agp_value(&[], 400, 1, -20_000);
        assert_eq!(a["days"], 90);
        assert_eq!(a["binMinutes"], 5);
        assert_eq!(a["tzOffset"], -14 * 60);
    }

    /// An empty window reports nulls (not fabricated zeros/GRI-0) — the same
    /// empty-safety the parent analytics module guarantees.
    #[test]
    fn empty_window_reports_nulls() {
        let v = analytics_value(&[], 24, 0, &TirThresholds::default());
        assert_eq!(v["n"], 0);
        assert!(v["meanMgdl"].is_null());
        assert!(v["gri"]["value"].is_null());
        assert!(v["timeInRangeWeighted"].is_null());
    }
}
