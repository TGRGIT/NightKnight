//! Glucose analytics — the numbers that tell a person how their management is going.
//!
//! Everything here computes on the canonical mg/dL inside [`GlucoseValue`], so a
//! stream that mixes mg/dL and mmol/L readings is analysed correctly. The formulas
//! follow the international consensus (ADA / ATTD "Clinical Targets for CGM Data
//! Interpretation", 2019) and are pinned to known reference values by tests.
//!
//! * **Time in Range (TIR)** — the share of readings in each glucose band. The
//!   single most actionable CGM metric. Bands (mg/dL): very-low `<54`, low `54–69`,
//!   target `70–180`, high `181–250`, very-high `>250`.
//! * **GMI** (Glucose Management Indicator) — an A1c-like % estimated from mean
//!   glucose: `GMI% = 3.31 + 0.02392 × mean_mg/dL`.
//! * **eA1c** — the older ADAG estimate: `(mean_mg/dL + 46.7) / 28.7`.
//! * **CV** (coefficient of variation) — variability; `≤ 36%` is considered stable.

use crate::timeutil::{self, DAY_MS};
use crate::units::GlucoseValue;

/// A timestamped glucose reading — the unit of all analytics and charting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlucoseReading {
    /// Time of the reading, epoch milliseconds (UTC).
    pub date_ms: i64,
    /// The reading itself, unit-aware.
    pub value: GlucoseValue,
}

impl GlucoseReading {
    pub fn new(date_ms: i64, value: GlucoseValue) -> Self {
        Self { date_ms, value }
    }
}

/// The glucose-band boundaries used for Time-in-Range, in mg/dL. Defaults are the
/// ADA/ATTD consensus values; `low`/`high` (the target range) are user-tunable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TirThresholds {
    /// Below this is "very low" (level-2 hypoglycaemia). Default 54.
    pub very_low: f64,
    /// `[very_low, low)` is "low"; this is the bottom of the target range. Default 70.
    pub low: f64,
    /// Top of the target range (inclusive). Default 180.
    pub high: f64,
    /// Above this is "very high" (level-2 hyperglycaemia). Default 250.
    pub very_high: f64,
}

impl Default for TirThresholds {
    fn default() -> Self {
        Self {
            very_low: 54.0,
            low: 70.0,
            high: 180.0,
            very_high: 250.0,
        }
    }
}

/// Which glucose band a single reading falls into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlucoseBand {
    VeryLow,
    Low,
    InRange,
    High,
    VeryHigh,
}

impl TirThresholds {
    /// Categorise one reading. Boundaries: target range is inclusive `[low, high]`;
    /// `low` band is `[very_low, low)`; `high` band is `(high, very_high]`.
    pub fn band(&self, mgdl: f64) -> GlucoseBand {
        if mgdl < self.very_low {
            GlucoseBand::VeryLow
        } else if mgdl < self.low {
            GlucoseBand::Low
        } else if mgdl <= self.high {
            GlucoseBand::InRange
        } else if mgdl <= self.very_high {
            GlucoseBand::High
        } else {
            GlucoseBand::VeryHigh
        }
    }
}

/// Time-in-Range result: the percentage of readings in each band (count-based, the
/// standard AGP method, which assumes roughly uniform CGM sampling). Percentages sum
/// to 100 when `n > 0`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct TimeInRange {
    pub n: usize,
    pub very_low_pct: f64,
    pub low_pct: f64,
    pub in_range_pct: f64,
    pub high_pct: f64,
    pub very_high_pct: f64,
}

impl TimeInRange {
    /// Compute TIR over a set of readings. An empty set yields all-zero with `n = 0`
    /// (never a division-by-zero or NaN — important for a UI that must not break on
    /// a sensor with no data).
    pub fn compute(readings: &[GlucoseReading], t: &TirThresholds) -> TimeInRange {
        let n = readings.len();
        if n == 0 {
            return TimeInRange::default();
        }
        let (mut vl, mut lo, mut ir, mut hi, mut vh) = (0usize, 0, 0, 0, 0);
        for r in readings {
            match t.band(r.value.mgdl()) {
                GlucoseBand::VeryLow => vl += 1,
                GlucoseBand::Low => lo += 1,
                GlucoseBand::InRange => ir += 1,
                GlucoseBand::High => hi += 1,
                GlucoseBand::VeryHigh => vh += 1,
            }
        }
        let pct = |c: usize| (c as f64) * 100.0 / (n as f64);
        TimeInRange {
            n,
            very_low_pct: pct(vl),
            low_pct: pct(lo),
            in_range_pct: pct(ir),
            high_pct: pct(hi),
            very_high_pct: pct(vh),
        }
    }

    /// Combined time spent below the target range (very-low + low).
    pub fn below_pct(&self) -> f64 {
        self.very_low_pct + self.low_pct
    }

    /// Combined time spent above the target range (high + very-high).
    pub fn above_pct(&self) -> f64 {
        self.high_pct + self.very_high_pct
    }
}

/// Mean glucose in mg/dL, or `None` for an empty set.
pub fn mean_mgdl(readings: &[GlucoseReading]) -> Option<f64> {
    if readings.is_empty() {
        return None;
    }
    let sum: f64 = readings.iter().map(|r| r.value.mgdl()).sum();
    Some(sum / readings.len() as f64)
}

/// **Sample** standard deviation of glucose in mg/dL (the `N − 1`, Bessel-corrected
/// form), or `None` if fewer than two readings (variability is undefined for a single
/// point). We use the sample form for parity with the reference CGM-variability tools
/// (the iglu R package, EasyGV, base-R `sd()`); at CGM series lengths it differs from
/// the population form by < 0.2%, but the choice is pinned here so SD/CV/J-index match
/// those tools' figures exactly.
pub fn std_dev_mgdl(readings: &[GlucoseReading]) -> Option<f64> {
    if readings.len() < 2 {
        return None;
    }
    let mean = mean_mgdl(readings)?;
    let var = readings
        .iter()
        .map(|r| {
            let d = r.value.mgdl() - mean;
            d * d
        })
        .sum::<f64>()
        / (readings.len() - 1) as f64;
    Some(var.sqrt())
}

/// Coefficient of variation (%) — `std_dev / mean × 100`. `≤ 36%` indicates stable
/// glucose per the consensus. `None` if it cannot be computed.
pub fn coefficient_of_variation(readings: &[GlucoseReading]) -> Option<f64> {
    let mean = mean_mgdl(readings)?;
    if mean == 0.0 {
        return None;
    }
    Some(std_dev_mgdl(readings)? / mean * 100.0)
}

/// Glucose Management Indicator (%), the modern A1c estimate, from mean mg/dL.
pub fn gmi_percent(mean_mgdl: f64) -> f64 {
    3.31 + 0.02392 * mean_mgdl
}

/// Estimated A1c (%) via the older ADAG regression, from mean mg/dL.
pub fn estimated_a1c_percent(mean_mgdl: f64) -> f64 {
    (mean_mgdl + 46.7) / 28.7
}

/// A bundled snapshot of the key metrics over a window of readings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlucoseSummary {
    pub n: usize,
    pub mean_mgdl: Option<f64>,
    /// Sample (`N − 1`) standard deviation of glucose in mg/dL (absolute spread) — see
    /// [`std_dev_mgdl`]. Surfaced per the consensus core set; pairs with
    /// [`cv_percent`](Self::cv_percent).
    pub sd_mgdl: Option<f64>,
    pub gmi_percent: Option<f64>,
    pub estimated_a1c_percent: Option<f64>,
    pub cv_percent: Option<f64>,
    pub tir: TimeInRange,
}

impl GlucoseSummary {
    /// Compute every metric in one pass-friendly call.
    pub fn compute(readings: &[GlucoseReading], thresholds: &TirThresholds) -> GlucoseSummary {
        let mean = mean_mgdl(readings);
        let sd = std_dev_mgdl(readings);
        GlucoseSummary {
            n: readings.len(),
            mean_mgdl: mean,
            sd_mgdl: sd,
            gmi_percent: mean.map(gmi_percent),
            estimated_a1c_percent: mean.map(estimated_a1c_percent),
            // CV = SD/mean·100; reuse the values we already have rather than re-summing.
            cv_percent: match (mean, sd) {
                (Some(m), Some(s)) if m != 0.0 => Some(s / m * 100.0),
                _ => None,
            },
            tir: TimeInRange::compute(readings, thresholds),
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────────
//  Extended analytics (the "Statistical Analysis" set)
//
//  Everything below is pure, unit-independent (computes on canonical mg/dL), and
//  gap-tolerant: it never assumes a fixed cadence, never panics on empty/sparse data,
//  and returns `Option`/empty rather than `NaN` when undersupplied. Each formula is
//  pinned to a reference value by the tests and cited to the source it comes from.
//  See `docs/STATISTICAL-ANALYSIS.md` for the research and rationale.
// ───────────────────────────────────────────────────────────────────────────────

/// The CGM sampling cadence we assume when none is known: 5 minutes (Dexcom; the AGP
/// reference cadence of 288 readings/day). Used only to estimate *expected* readings
/// for the data-sufficiency percentage — never to fabricate data.
pub const DEFAULT_CADENCE_MS: i64 = 5 * 60_000;

/// The largest inter-reading interval we treat as continuous data for **time-weighted**
/// metrics: a longer gap is a sensor outage and accrues no time. 15 minutes tolerates a
/// few dropped 5-minute samples and exactly matches the Libre 15-minute cadence (the
/// comparison is strictly greater-than).
pub const DEFAULT_MAX_GAP_MS: i64 = 15 * 60_000;

/// The gap beyond which an **episode** is not treated as continuous — a longer interval
/// with no readings closes any open event rather than bridging it (2× a 15-minute
/// cadence; the consensus discontinuity guidance is "> 2× expected cadence"). More
/// lenient than [`DEFAULT_MAX_GAP_MS`] because an event legitimately spans the odd
/// missed sample, but never an unmonitored half-hour.
pub const DEFAULT_EPISODE_GAP_MS: i64 = 30 * 60_000;

/// Minimum duration for a glucose excursion to count as a clinical *event*: 15 minutes
/// beyond the threshold (2019 consensus / Danne 2017).
pub const MIN_EPISODE_MS: i64 = 15 * 60_000;

/// How long glucose must stay recovered (back across the threshold) before an event is
/// considered ended — so a brief blip back into range does not split one event in two
/// (2019 consensus). 15 minutes.
pub const EPISODE_RECOVERY_MS: i64 = 15 * 60_000;

/// Recommended minimum days of data for the metrics to be trustworthy (2019 consensus).
pub const RECOMMENDED_DAYS: usize = 14;

/// Recommended minimum % of the window with active CGM data (2019 consensus).
pub const RECOMMENDED_ACTIVE_PCT: f64 = 70.0;

/// The `p`-th percentile (`p` in `0..=100`) of `values` by **linear interpolation
/// between closest ranks** — the "type 7" method used by NumPy and R's default. The
/// slice need not be pre-sorted. Returns `None` for an empty slice; non-finite values
/// are ignored. This is the percentile used for the AGP bands.
pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if v.len() == 1 {
        return Some(v[0]);
    }
    let rank = (p.clamp(0.0, 100.0) / 100.0) * (v.len() as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let frac = rank - lo as f64;
    Some(v[lo] + (v[hi] - v[lo]) * frac)
}

/// Readings sorted ascending by time (a small owned copy; `GlucoseReading` is `Copy`).
fn sorted_by_time(readings: &[GlucoseReading]) -> Vec<GlucoseReading> {
    let mut v = readings.to_vec();
    v.sort_by_key(|r| r.date_ms);
    v
}

/// How much data the metrics are based on — the caption every consensus CGM report
/// leads with, because "85% in range" over six hours is not the same claim as over two
/// weeks. All fields degrade gracefully on empty input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coverage {
    /// Number of readings in the window.
    pub n: usize,
    /// Earliest / latest reading time in the window (epoch ms), if any.
    pub first_ms: Option<i64>,
    pub last_ms: Option<i64>,
    /// Span actually covered, `(last − first)`, in days.
    pub days_covered: f64,
    /// Count of distinct local calendar days that have at least one reading.
    pub distinct_days: usize,
    /// Expected readings for the window at the assumed cadence (`window / cadence`).
    pub expected: f64,
    /// `100 × n / expected`, clamped to `[0, 100]` — the consensus "% time active".
    pub percent_active: f64,
    /// Whether the data clears the consensus bar (≥ 14 distinct days **and**
    /// ≥ 70% active). When false, the UI should caveat the headline metrics.
    pub sufficient: bool,
}

impl Coverage {
    /// Summarise coverage of a `window_ms`-long window sampled at `cadence_ms`, with
    /// local calendar days counted in the `utc_offset_min` timezone.
    pub fn compute(
        readings: &[GlucoseReading],
        window_ms: i64,
        cadence_ms: i64,
        utc_offset_min: i64,
    ) -> Coverage {
        let n = readings.len();
        let first = readings.iter().map(|r| r.date_ms).min();
        let last = readings.iter().map(|r| r.date_ms).max();
        let days_covered = match (first, last) {
            (Some(a), Some(b)) => (b - a) as f64 / DAY_MS as f64,
            _ => 0.0,
        };
        let mut days: Vec<i64> = readings
            .iter()
            .map(|r| timeutil::day_number(r.date_ms, utc_offset_min))
            .collect();
        days.sort_unstable();
        days.dedup();
        let distinct_days = days.len();
        let expected = if cadence_ms > 0 {
            window_ms as f64 / cadence_ms as f64
        } else {
            0.0
        };
        // "% time active" counts distinct cadence-sized epochs (5-min slots) that hold at
        // least one reading — NOT raw readings. Counting readings against an assumed
        // cadence pins the metric at 100% for any denser-than-cadence stream (e.g. 1-min
        // Nightscout data), hiding real gaps; binning into slots is density-insensitive,
        // caps naturally at 100%, and lets genuine drop-outs show through.
        let active_slots = if cadence_ms > 0 {
            let mut slots: Vec<i64> =
                readings.iter().map(|r| r.date_ms.div_euclid(cadence_ms)).collect();
            slots.sort_unstable();
            slots.dedup();
            slots.len()
        } else {
            0
        };
        let percent_active = if expected > 0.0 {
            (100.0 * active_slots as f64 / expected).clamp(0.0, 100.0)
        } else {
            0.0
        };
        Coverage {
            n,
            first_ms: first,
            last_ms: last,
            days_covered,
            distinct_days,
            expected,
            percent_active,
            sufficient: distinct_days >= RECOMMENDED_DAYS && percent_active >= RECOMMENDED_ACTIVE_PCT,
        }
    }
}

impl TimeInRange {
    /// **Time-weighted** Time-in-Range: instead of counting readings (which over-weights
    /// densely-sampled stretches), attribute the *duration* between each consecutive
    /// pair of readings to the earlier reading's band, skipping any interval longer than
    /// `max_gap_ms` (a sensor gap accrues no time). Percentages are of the total accrued
    /// time. Returns `None` when no interval is short enough to accrue (e.g. a single
    /// reading, or only isolated points). The headline TIR stays count-based
    /// ([`TimeInRange::compute`]); this is the gap-robust refinement.
    pub fn compute_weighted(
        readings: &[GlucoseReading],
        t: &TirThresholds,
        max_gap_ms: i64,
    ) -> Option<TimeInRange> {
        let rs = sorted_by_time(readings);
        if rs.len() < 2 {
            return None;
        }
        let (mut vl, mut lo, mut ir, mut hi, mut vh) = (0i64, 0i64, 0i64, 0i64, 0i64);
        let mut total = 0i64;
        for pair in rs.windows(2) {
            let dt = pair[1].date_ms - pair[0].date_ms;
            if dt <= 0 || dt > max_gap_ms {
                continue; // duplicate timestamp or sensor gap → no accrual
            }
            total += dt;
            match t.band(pair[0].value.mgdl()) {
                GlucoseBand::VeryLow => vl += dt,
                GlucoseBand::Low => lo += dt,
                GlucoseBand::InRange => ir += dt,
                GlucoseBand::High => hi += dt,
                GlucoseBand::VeryHigh => vh += dt,
            }
        }
        if total == 0 {
            return None;
        }
        let pct = |c: i64| c as f64 * 100.0 / total as f64;
        Some(TimeInRange {
            n: rs.len(),
            very_low_pct: pct(vl),
            low_pct: pct(lo),
            in_range_pct: pct(ir),
            high_pct: pct(hi),
            very_high_pct: pct(vh),
        })
    }
}

/// A GRI risk zone (best → worst). The quintile bands of the Glycemia Risk Index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GriZone {
    A,
    B,
    C,
    D,
    E,
}

impl GriZone {
    /// The zone for a GRI value (0–100). Boundaries are the 20/40/60/80 quintile cut
    /// points (Klonoff et al.): A ≤20, B ≤40, C ≤60, D ≤80, E >80.
    pub fn from_gri(gri: f64) -> GriZone {
        match gri {
            g if g <= 20.0 => GriZone::A,
            g if g <= 40.0 => GriZone::B,
            g if g <= 60.0 => GriZone::C,
            g if g <= 80.0 => GriZone::D,
            _ => GriZone::E,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GriZone::A => "A",
            GriZone::B => "B",
            GriZone::C => "C",
            GriZone::D => "D",
            GriZone::E => "E",
        }
    }
}

/// **Glycemia Risk Index** (Klonoff, Wang, Rodbard et al., *J Diabetes Sci Technol*
/// 2023): a single 0–100 composite (lower is better) that blends hypo- and
/// hyper-glycaemia risk using weights derived from 330 clinicians' rankings, so it
/// tracks clinical perception of risk better than TIR alone. It reuses exactly the
/// five consensus bands we already compute.
///
/// ```text
/// Hypo  = VLow + 0.8·Low      (VLow = %<54,  Low = %54–69)
/// Hyper = VHigh + 0.5·High    (VHigh = %>250, High = %181–250)
/// GRI   = (3.0·Hypo) + (1.6·Hyper)         capped at 100
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlycemiaRiskIndex {
    /// The 0–100 index (lower = better).
    pub gri: f64,
    /// `VLow + 0.8·Low` — the hypoglycaemia component (the y-axis of the 2-D GRI grid).
    pub hypo_component: f64,
    /// `VHigh + 0.5·High` — the hyperglycaemia component (the x-axis).
    pub hyper_component: f64,
    /// The A–E risk zone.
    pub zone: GriZone,
}

impl GlycemiaRiskIndex {
    /// Compute the GRI from a (count- or time-weighted) Time-in-Range distribution.
    pub fn from_tir(tir: &TimeInRange) -> GlycemiaRiskIndex {
        let hypo = tir.very_low_pct + 0.8 * tir.low_pct;
        let hyper = tir.very_high_pct + 0.5 * tir.high_pct;
        let gri = (3.0 * hypo + 1.6 * hyper).min(100.0);
        GlycemiaRiskIndex {
            gri,
            hypo_component: hypo,
            hyper_component: hyper,
            zone: GriZone::from_gri(gri),
        }
    }
}

/// A detected glucose excursion (a hypo- or hyper-glycaemic *event*).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlucoseEpisode {
    /// First reading beyond the threshold (epoch ms).
    pub start_ms: i64,
    /// Last reading beyond the threshold (epoch ms).
    pub end_ms: i64,
    /// Duration the glucose was beyond the threshold, in minutes (`end − start`).
    pub duration_min: f64,
    /// Nadir (for lows) or peak (for highs) reached during the event, mg/dL.
    pub extreme_mgdl: f64,
    /// Whether the event started in the nocturnal window (00:00–06:00 local).
    pub nocturnal: bool,
}

/// Detect glucose excursions beyond `threshold_mgdl`. `below = true` finds hypo events
/// (glucose strictly *under* the threshold); `false` finds hyper events (strictly
/// *over* it) — a reading exactly on the threshold is recovery / in-range, never
/// in-event (the strict `<` / `>` event edge from Danne 2017 / ONWARDS).
///
/// Following the consensus definition, an event runs from the first beyond reading
/// (onset) until glucose **returns across the threshold** (recovery), and is reported
/// only when that span is ≥ [`MIN_EPISODE_MS`]. A momentary blip back across the
/// threshold for less than [`EPISODE_RECOVERY_MS`] does *not* end the event (the
/// anti-sawtooth hysteresis), so one excursion is never split in two. A data gap longer
/// than `max_gap_ms` (see [`DEFAULT_EPISODE_GAP_MS`]) closes the event — we never claim
/// it continued through an unmonitored interval.
///
/// `extreme_mgdl` is the nadir (lows) / peak (highs) over the beyond samples. Readings
/// may be in any order. To get *severe* (level-2) events, call with the 54 / 250
/// threshold; that result is the subset of the 70 / 180 events that also breached it.
pub fn detect_episodes(
    readings: &[GlucoseReading],
    threshold_mgdl: f64,
    below: bool,
    utc_offset_min: i64,
    max_gap_ms: i64,
) -> Vec<GlucoseEpisode> {
    let rs = sorted_by_time(readings);
    let beyond = |mgdl: f64| if below { mgdl < threshold_mgdl } else { mgdl > threshold_mgdl };
    let mut events = Vec::new();
    let n = rs.len();
    let mut i = 0;
    while i < n {
        if !beyond(rs[i].value.mgdl()) {
            i += 1;
            continue;
        }
        // Open an excursion at i and extend it over the beyond run (absorbing brief
        // in-range blips shorter than the recovery window).
        let start = rs[i].date_ms;
        let mut last_beyond = i;
        let mut extreme = rs[i].value.mgdl();
        let mut k = i + 1;
        while k < n {
            if rs[k].date_ms - rs[k - 1].date_ms > max_gap_ms {
                break; // sensor gap → the excursion cannot be claimed to continue
            }
            let v = rs[k].value.mgdl();
            if beyond(v) {
                last_beyond = k;
                extreme = if below { extreme.min(v) } else { extreme.max(v) };
            } else if rs[k].date_ms - rs[last_beyond].date_ms >= EPISODE_RECOVERY_MS {
                break; // sustained recovery → event ended
            }
            k += 1;
        }
        // The event ends when glucose first returns across the threshold: the reading
        // immediately after the last beyond one, if it exists and is within a gap (it
        // is in-range by construction). Otherwise the event is right-censored at the
        // last beyond reading (a gap or the end of the series). Duration is onset →
        // recovery, matching the consensus "until glucose returns to range" definition.
        let recovered = last_beyond + 1 < n
            && rs[last_beyond + 1].date_ms - rs[last_beyond].date_ms <= max_gap_ms;
        let end = if recovered {
            rs[last_beyond + 1].date_ms
        } else {
            rs[last_beyond].date_ms
        };
        if end - start >= MIN_EPISODE_MS {
            let m = timeutil::minute_of_day(start, utc_offset_min);
            events.push(GlucoseEpisode {
                start_ms: start,
                end_ms: end,
                duration_min: (end - start) as f64 / 60_000.0,
                extreme_mgdl: extreme,
                nocturnal: (0..6 * 60).contains(&m),
            });
        }
        i = k.max(i + 1);
    }
    events
}

/// A roll-up of a set of episodes for one threshold, for the "Episodes" card.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct EpisodeSummary {
    pub count: usize,
    pub nocturnal_count: usize,
    pub per_day: f64,
    pub longest_min: f64,
    pub total_min: f64,
}

impl EpisodeSummary {
    /// Summarise episodes over `days` of data (used to normalise events/day). `days`
    /// of 0 yields a `per_day` of 0 rather than a division by zero.
    pub fn of(episodes: &[GlucoseEpisode], days: f64) -> EpisodeSummary {
        let count = episodes.len();
        let nocturnal_count = episodes.iter().filter(|e| e.nocturnal).count();
        let longest_min = episodes.iter().map(|e| e.duration_min).fold(0.0, f64::max);
        let total_min = episodes.iter().map(|e| e.duration_min).sum();
        let per_day = if days > 0.0 { count as f64 / days } else { 0.0 };
        EpisodeSummary { count, nocturnal_count, per_day, longest_min, total_min }
    }
}

/// One time-of-day bin of the Ambulatory Glucose Profile: the glucose distribution at
/// this slot of the day, pooled across every day in the window. Empty bins carry
/// `n = 0` and `None` percentiles so the client can draw a continuous 24-hour axis and
/// skip the gaps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AgpBin {
    /// Minute-of-day the bin starts at (local), `0..1440`.
    pub minute_of_day: i64,
    pub n: usize,
    pub p05: Option<f64>,
    pub p25: Option<f64>,
    pub p50: Option<f64>,
    pub p75: Option<f64>,
    pub p95: Option<f64>,
}

/// Build the **Ambulatory Glucose Profile**: overlay every day in the window onto a
/// single 24-hour axis split into `bin_minutes`-wide bins (default 15 → 96 bins), and
/// for each bin report the 5/25/50/75/95 glucose percentiles (Bergenstal AGP). Returns
/// one entry per bin across the whole day, in order. `bin_minutes` is clamped to a
/// divisor-friendly `1..=720`.
pub fn agp_bins(readings: &[GlucoseReading], bin_minutes: i64, utc_offset_min: i64) -> Vec<AgpBin> {
    let bin = bin_minutes.clamp(1, 720);
    let count = (1440 / bin).max(1) as usize;
    let mut buckets: Vec<Vec<f64>> = vec![Vec::new(); count];
    for r in readings {
        let m = timeutil::minute_of_day(r.date_ms, utc_offset_min);
        let idx = ((m / bin) as usize).min(count - 1);
        buckets[idx].push(r.value.mgdl());
    }
    buckets
        .into_iter()
        .enumerate()
        .map(|(i, vals)| AgpBin {
            minute_of_day: i as i64 * bin,
            n: vals.len(),
            p05: percentile(&vals, 5.0),
            p25: percentile(&vals, 25.0),
            p50: percentile(&vals, 50.0),
            p75: percentile(&vals, 75.0),
            p95: percentile(&vals, 95.0),
        })
        .collect()
}

/// Stats for one part of the day (e.g. overnight) — a [`GlucoseSummary`] tagged with
/// the local hour range it covers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeriodStats {
    /// Local start hour (inclusive) and end hour (exclusive), 0–24.
    pub start_hour: i64,
    pub end_hour: i64,
    pub summary: GlucoseSummary,
}

/// The four standard time-of-day periods — overnight (00–06), morning (06–12),
/// afternoon (12–18), evening (18–24) — each summarised with the core metrics, to
/// surface patterns like the dawn phenomenon or post-dinner highs. Readings are bucketed
/// by **local** hour using `utc_offset_min`.
pub fn time_of_day_patterns(
    readings: &[GlucoseReading],
    thresholds: &TirThresholds,
    utc_offset_min: i64,
) -> Vec<PeriodStats> {
    [(0, 6), (6, 12), (12, 18), (18, 24)]
        .into_iter()
        .map(|(start_hour, end_hour)| {
            let group: Vec<GlucoseReading> = readings
                .iter()
                .copied()
                .filter(|r| {
                    let h = timeutil::minute_of_day(r.date_ms, utc_offset_min) / 60;
                    h >= start_hour && h < end_hour
                })
                .collect();
            PeriodStats {
                start_hour,
                end_hour,
                summary: GlucoseSummary::compute(&group, thresholds),
            }
        })
        .collect()
}

/// **J-index** (Wojcicki 1995): a single severity score combining the mean and the SD,
/// `J = 0.001 × (mean + SD)²` with both terms in **mg/dL**. Non-diabetic reference
/// range ≈ 4.7–23.6. Returns `None` if either input is missing.
pub fn j_index(mean_mgdl: Option<f64>, sd_mgdl: Option<f64>) -> Option<f64> {
    match (mean_mgdl, sd_mgdl) {
        (Some(m), Some(s)) => Some(0.001 * (m + s).powi(2)),
        _ => None,
    }
}

/// **MAGE** — Mean Amplitude of Glycemic Excursions (Service 1970): the mean size of
/// the *meaningful* swings, i.e. the peak-to-nadir amplitudes between successive
/// turning points that exceed **1 SD** of the glucose. Smaller daily SD-sized wobble is
/// ignored, so MAGE captures the excursions a person actually feels.
///
/// Algorithm (a widely-used deterministic form): walk the time-ordered series, collect
/// the local turning points (including the endpoints), then average the absolute
/// amplitudes between consecutive turning points that are larger than 1 SD. Returns
/// `None` if there are too few readings, or no swing exceeds 1 SD. MAGE is known to be
/// algorithm-sensitive — this is a stable, documented variant, not the only one.
pub fn mage(readings: &[GlucoseReading]) -> Option<f64> {
    let sd = std_dev_mgdl(readings)?;
    if sd <= 0.0 {
        return None;
    }
    let rs = sorted_by_time(readings);
    let v: Vec<f64> = rs.iter().map(|r| r.value.mgdl()).collect();
    if v.len() < 3 {
        return None;
    }
    // Turning points: endpoints plus every local extremum.
    let mut turns = vec![v[0]];
    for i in 1..v.len() - 1 {
        let up_peak = v[i] > v[i - 1] && v[i] >= v[i + 1];
        let down_trough = v[i] < v[i - 1] && v[i] <= v[i + 1];
        if up_peak || down_trough {
            turns.push(v[i]);
        }
    }
    turns.push(v[v.len() - 1]);
    let amps: Vec<f64> = turns
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .filter(|&a| a > sd)
        .collect();
    if amps.is_empty() {
        return None;
    }
    Some(amps.iter().sum::<f64>() / amps.len() as f64)
}

/// Find the reading closest to `target_ms`, returning its mg/dL if within `tolerance_ms`
/// (the time-ordered list `rs` enables a binary search). Used by the lag-based metrics
/// (CONGA, MODD) so they tolerate gaps and non-uniform cadence — a reading n hours / one
/// day earlier need only exist *approximately*.
fn nearest_within(rs: &[GlucoseReading], target_ms: i64, tolerance_ms: i64) -> Option<f64> {
    if rs.is_empty() {
        return None;
    }
    let pos = rs.partition_point(|r| r.date_ms < target_ms);
    let mut best: Option<(i64, f64)> = None;
    for idx in [pos.wrapping_sub(1), pos] {
        if idx < rs.len() {
            let r = rs[idx];
            let d = (r.date_ms - target_ms).abs();
            if d <= tolerance_ms && best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, r.value.mgdl()));
            }
        }
    }
    best.map(|(_, mgdl)| mgdl)
}

/// **CONGA(n)** — Continuous Overall Net Glycemic Action at lag `n` hours (McDonnell
/// 2005): the standard deviation of the differences between each reading and the reading
/// `n` hours earlier. A within-day variability measure. Gap-tolerant: the earlier
/// partner is matched within `tolerance_ms`; pairs without a partner are skipped.
/// Returns `None` with fewer than two valid pairs.
pub fn conga(readings: &[GlucoseReading], hours: f64, tolerance_ms: i64) -> Option<f64> {
    let rs = sorted_by_time(readings);
    let lag_ms = (hours * 3_600_000.0) as i64;
    let diffs: Vec<f64> = rs
        .iter()
        .filter_map(|r| {
            nearest_within(&rs, r.date_ms - lag_ms, tolerance_ms).map(|past| r.value.mgdl() - past)
        })
        .collect();
    sample_sd(&diffs)
}

/// **MODD** — Mean Of Daily Differences (Molnar 1972): the mean *absolute* difference
/// between glucose readings taken at the same time of day on consecutive days. A
/// day-to-day reproducibility measure. Gap-tolerant via the same nearest-match (24 h
/// earlier within `tolerance_ms`). Returns `None` with no valid pairs.
pub fn modd(readings: &[GlucoseReading], tolerance_ms: i64) -> Option<f64> {
    let rs = sorted_by_time(readings);
    let diffs: Vec<f64> = rs
        .iter()
        .filter_map(|r| {
            nearest_within(&rs, r.date_ms - DAY_MS, tolerance_ms)
                .map(|prev| (r.value.mgdl() - prev).abs())
        })
        .collect();
    if diffs.is_empty() {
        return None;
    }
    Some(diffs.iter().sum::<f64>() / diffs.len() as f64)
}

/// Sample (`N − 1`) standard deviation of a plain slice, or `None` with fewer than two
/// values. CONGA is defined as the sample SD of the lagged differences (McDonnell 2005 /
/// iglu), matching [`std_dev_mgdl`]'s Bessel-corrected convention.
fn sample_sd(values: &[f64]) -> Option<f64> {
    if values.len() < 2 {
        return None;
    }
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let var = values.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1.0);
    Some(var.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::GlucoseValue;

    fn mgdl(v: f64) -> GlucoseReading {
        GlucoseReading::new(0, GlucoseValue::from_mgdl(v).unwrap())
    }

    /// Band edges must match the consensus exactly: 70 and 180 are *in range*, 54 is
    /// the very-low edge, 250 is the top of "high". Mis-binning these would misstate
    /// how much time a person spends in a dangerous zone.
    #[test]
    fn band_boundaries_match_consensus() {
        let t = TirThresholds::default();
        assert_eq!(t.band(53.9), GlucoseBand::VeryLow);
        assert_eq!(t.band(54.0), GlucoseBand::Low);
        assert_eq!(t.band(69.9), GlucoseBand::Low);
        assert_eq!(t.band(70.0), GlucoseBand::InRange);
        assert_eq!(t.band(180.0), GlucoseBand::InRange);
        assert_eq!(t.band(180.1), GlucoseBand::High);
        assert_eq!(t.band(250.0), GlucoseBand::High);
        assert_eq!(t.band(250.1), GlucoseBand::VeryHigh);
    }

    /// A worked TIR example. 10 readings spread across the bands; percentages must be
    /// exact and sum to 100.
    #[test]
    fn time_in_range_percentages_are_exact_and_sum_to_100() {
        let readings = vec![
            mgdl(40.0),  // very low
            mgdl(60.0),  // low
            mgdl(80.0),  // in range
            mgdl(100.0), // in range
            mgdl(120.0), // in range
            mgdl(150.0), // in range
            mgdl(170.0), // in range
            mgdl(200.0), // high
            mgdl(230.0), // high
            mgdl(300.0), // very high
        ];
        let tir = TimeInRange::compute(&readings, &TirThresholds::default());
        assert_eq!(tir.n, 10);
        assert_eq!(tir.very_low_pct, 10.0);
        assert_eq!(tir.low_pct, 10.0);
        assert_eq!(tir.in_range_pct, 50.0);
        assert_eq!(tir.high_pct, 20.0);
        assert_eq!(tir.very_high_pct, 10.0);
        let total = tir.very_low_pct + tir.low_pct + tir.in_range_pct + tir.high_pct + tir.very_high_pct;
        assert!((total - 100.0).abs() < 1e-9);
        assert_eq!(tir.below_pct(), 20.0);
        assert_eq!(tir.above_pct(), 30.0);
    }

    /// Empty input must not panic or produce NaN — a freshly-set-up sensor has no
    /// data yet, and the dashboard still has to render.
    #[test]
    fn empty_dataset_is_safe() {
        let tir = TimeInRange::compute(&[], &TirThresholds::default());
        assert_eq!(tir, TimeInRange::default());
        assert_eq!(mean_mgdl(&[]), None);
        assert_eq!(std_dev_mgdl(&[]), None);
        assert_eq!(coefficient_of_variation(&[]), None);
    }

    /// GMI and eA1c at a mean of 154 mg/dL both land at ~7.0% — the canonical
    /// reference point (A1c 7% ↔ mean 154 mg/dL).
    #[test]
    fn gmi_and_ea1c_match_reference_point() {
        let mean = 154.0;
        assert!((gmi_percent(mean) - 6.99).abs() < 0.01, "GMI was {}", gmi_percent(mean));
        assert!(
            (estimated_a1c_percent(mean) - 6.99).abs() < 0.01,
            "eA1c was {}",
            estimated_a1c_percent(mean)
        );
        // Additional GMI reference points from the dossier table (Bergenstal 2018).
        assert!((gmi_percent(100.0) - 5.702).abs() < 0.001);
        assert!((gmi_percent(200.0) - 8.094).abs() < 0.001);
        assert!((gmi_percent(300.0) - 10.486).abs() < 0.001);
    }

    /// Mean, SD and CV on a tiny known set.
    #[test]
    fn mean_sd_cv_on_known_set() {
        // values 90, 110, 100 → mean 100; sample variance = (100+100+0)/(3−1) = 100.
        let readings = vec![mgdl(90.0), mgdl(110.0), mgdl(100.0)];
        assert_eq!(mean_mgdl(&readings), Some(100.0));
        let sd = std_dev_mgdl(&readings).unwrap();
        assert!((sd - 10.0).abs() < 1e-9, "sample SD should be 10, got {sd}");
        let cv = coefficient_of_variation(&readings).unwrap();
        assert!((cv - sd).abs() < 1e-9); // mean is 100, so CV% == sd numerically
    }

    /// Analytics must be unit-blind: the *same physical concentration* entered via
    /// mmol/L lands in the same band as via mg/dL. We build the mmol readings by
    /// converting the mg/dL ones, so the canonical concentration is identical, and we
    /// pick mid-band values (away from the 54/70/180/250 edges) — because at an exact
    /// boundary, display-equivalent figures like "10.0 mmol/L" (= 180.156 mg/dL) and
    /// "180 mg/dL" are deliberately *not* the same concentration. (Per-unit threshold
    /// handling for edge cases is a settings-layer concern, not core analytics.)
    #[test]
    fn analytics_are_unit_independent() {
        let mgdl_values = [45.0, 120.0, 270.0]; // very-low, in-range, very-high
        let mgdl_readings: Vec<_> = mgdl_values.iter().map(|&v| mgdl(v)).collect();
        let mmol_readings: Vec<_> = mgdl_values
            .iter()
            .map(|&v| {
                let mmol = crate::units::mgdl_to_mmol(v);
                GlucoseReading::new(0, GlucoseValue::from_mmol(mmol).unwrap())
            })
            .collect();
        let a = TimeInRange::compute(&mgdl_readings, &TirThresholds::default());
        let b = TimeInRange::compute(&mmol_readings, &TirThresholds::default());
        assert_eq!(a, b);
    }

    // ── extended analytics ──────────────────────────────────────────────────────

    /// A reading at `min` minutes past epoch with value `v` mg/dL.
    fn at(min: i64, v: f64) -> GlucoseReading {
        GlucoseReading::new(min * 60_000, GlucoseValue::from_mgdl(v).unwrap())
    }
    /// A reading at an absolute epoch `ms`.
    fn at_ms(ms: i64, v: f64) -> GlucoseReading {
        GlucoseReading::new(ms, GlucoseValue::from_mgdl(v).unwrap())
    }
    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// Percentiles use type-7 linear interpolation, matching NumPy/R. Pinned on a known
    /// set so the AGP bands can't silently shift method.
    #[test]
    fn percentile_is_type7_interpolation() {
        let v = [10.0, 20.0, 30.0, 40.0, 50.0];
        assert_eq!(percentile(&v, 50.0), Some(30.0));
        assert_eq!(percentile(&v, 25.0), Some(20.0));
        assert_eq!(percentile(&v, 75.0), Some(40.0));
        assert!(close(percentile(&v, 5.0).unwrap(), 12.0)); // rank 0.2 → 10 + 0.2·10
        assert!(close(percentile(&v, 95.0).unwrap(), 48.0)); // rank 3.8 → 40 + 0.8·10
        assert_eq!(percentile(&[42.0], 50.0), Some(42.0)); // single value
        assert_eq!(percentile(&[], 50.0), None); // empty is safe
    }

    /// Coverage reports the honest fraction of the window that has data, and gates the
    /// "trustworthy" flag on the consensus bar (≥14 days AND ≥70% active).
    #[test]
    fn coverage_reports_percent_active_and_sufficiency() {
        // 6 readings, 5 min apart, in a 1-hour window at 5-min cadence (expected 12).
        let readings: Vec<_> = (0..6).map(|i| at(i * 5, 100.0)).collect();
        let cov = Coverage::compute(&readings, 60 * 60_000, DEFAULT_CADENCE_MS, 0);
        assert_eq!(cov.n, 6);
        assert_eq!(cov.expected, 12.0);
        assert!(close(cov.percent_active, 50.0));
        assert_eq!(cov.distinct_days, 1);
        assert!(!cov.sufficient, "one day, 50% active is not sufficient");
        // Denser-than-cadence data (1-min) no longer pins at 100%: these 50 readings span
        // minutes 0–49, i.e. 10 of the 12 five-minute slots in the hour, so coverage is
        // ~83% — the real gap at the end of the window shows through instead of being
        // masked by the raw count.
        let dense: Vec<_> = (0..50).map(|i| at(i, 100.0)).collect();
        let cov2 = Coverage::compute(&dense, 60 * 60_000, DEFAULT_CADENCE_MS, 0);
        assert!(close(cov2.percent_active, 100.0 / 12.0 * 10.0), "got {}", cov2.percent_active);
        // A truly full hour of 1-min data fills every slot → exactly 100%, never over.
        let full: Vec<_> = (0..60).map(|i| at(i, 100.0)).collect();
        let cov3 = Coverage::compute(&full, 60 * 60_000, DEFAULT_CADENCE_MS, 0);
        assert_eq!(cov3.percent_active, 100.0);
        // Empty data never panics.
        let empty = Coverage::compute(&[], 60 * 60_000, DEFAULT_CADENCE_MS, 0);
        assert_eq!(empty.percent_active, 0.0);
        assert_eq!(empty.distinct_days, 0);
        assert_eq!(empty.first_ms, None);
    }

    /// Time-weighting attributes *duration* to bands, so a dense in-range cluster does
    /// not out-vote a sparser excursion the way count-weighting does, and a long gap
    /// accrues no time. Here count-TIR is 50/50 but time-TIR is 66.7/33.3.
    #[test]
    fn time_weighted_tir_uses_duration_not_count() {
        let readings = [at(0, 100.0), at(5, 100.0), at(10, 60.0), at(15, 60.0)];
        let t = TirThresholds::default();
        let count = TimeInRange::compute(&readings, &t);
        assert_eq!(count.in_range_pct, 50.0);
        let w = TimeInRange::compute_weighted(&readings, &t, DEFAULT_MAX_GAP_MS).unwrap();
        assert!(close(w.in_range_pct, 200.0 / 3.0), "got {}", w.in_range_pct);
        assert!(close(w.low_pct, 100.0 / 3.0));
        // A gap longer than max_gap accrues no time across it.
        let gapped = [at(0, 100.0), at(60, 60.0)]; // 60-min gap
        assert_eq!(TimeInRange::compute_weighted(&gapped, &t, DEFAULT_MAX_GAP_MS), None);
        // A single reading has no interval to weight.
        assert_eq!(TimeInRange::compute_weighted(&readings[..1], &t, DEFAULT_MAX_GAP_MS), None);
    }

    /// The GRI worked example and its expanded form must agree, the zone must follow
    /// the quintile cut points, and the index must cap at 100.
    #[test]
    fn gri_matches_worked_example_and_zones() {
        // The Klonoff et al. paper's own worked example (5/10/50/20/15 → 79.0) — the
        // primary regression anchor; both code paths must give bit-identical 79.0.
        let paper = TimeInRange {
            n: 100,
            very_low_pct: 5.0,
            low_pct: 10.0,
            in_range_pct: 50.0,
            high_pct: 20.0,
            very_high_pct: 15.0,
        };
        let pg = GlycemiaRiskIndex::from_tir(&paper);
        assert!(close(pg.hypo_component, 13.0)); // 5 + 0.8·10
        assert!(close(pg.hyper_component, 25.0)); // 15 + 0.5·20
        assert!(close(pg.gri, 79.0)); // 3·13 + 1.6·25
        assert_eq!(pg.zone, GriZone::D);

        let tir = TimeInRange {
            n: 100,
            very_low_pct: 10.0,
            low_pct: 10.0,
            in_range_pct: 70.0,
            high_pct: 5.0,
            very_high_pct: 5.0,
        };
        let g = GlycemiaRiskIndex::from_tir(&tir);
        // hypo = 10 + 0.8·10 = 18; hyper = 5 + 0.5·5 = 7.5; GRI = 3·18 + 1.6·7.5 = 66.
        assert!(close(g.hypo_component, 18.0));
        assert!(close(g.hyper_component, 7.5));
        assert!(close(g.gri, 66.0));
        // Expanded form: 3·VLow + 2.4·Low + 1.6·VHigh + 0.8·High (algebraically equal).
        let expanded = 3.0 * 10.0 + 2.4 * 10.0 + 1.6 * 5.0 + 0.8 * 5.0;
        assert!(close(g.gri, expanded));
        assert_eq!(g.zone, GriZone::D); // 66 ∈ (60, 80]
        // A perfect run is GRI 0, zone A; an all-very-low run caps at 100, zone E.
        let perfect = TimeInRange { n: 1, in_range_pct: 100.0, ..Default::default() };
        assert_eq!(GlycemiaRiskIndex::from_tir(&perfect).gri, 0.0);
        assert_eq!(GlycemiaRiskIndex::from_tir(&perfect).zone, GriZone::A);
        let worst = TimeInRange { n: 1, very_low_pct: 100.0, ..Default::default() };
        assert_eq!(GlycemiaRiskIndex::from_tir(&worst).gri, 100.0); // 3·100 capped
        assert_eq!(GlycemiaRiskIndex::from_tir(&worst).zone, GriZone::E);
    }

    /// Zone boundaries resolve at the exact quintile edges (≤ is inclusive).
    #[test]
    fn gri_zone_boundaries() {
        assert_eq!(GriZone::from_gri(0.0), GriZone::A);
        assert_eq!(GriZone::from_gri(20.0), GriZone::A);
        assert_eq!(GriZone::from_gri(20.01), GriZone::B);
        assert_eq!(GriZone::from_gri(40.0), GriZone::B);
        assert_eq!(GriZone::from_gri(60.0), GriZone::C);
        assert_eq!(GriZone::from_gri(80.0), GriZone::D);
        assert_eq!(GriZone::from_gri(80.01), GriZone::E);
        assert_eq!(GriZone::from_gri(100.0), GriZone::E);
    }

    /// Readings from `(minute-offset, mg/dL)` pairs at a fixed daytime base (noon UTC),
    /// so events are non-nocturnal unless a test says otherwise.
    fn series(pairs: &[(i64, f64)]) -> Vec<GlucoseReading> {
        let base = 12 * 3_600_000;
        pairs.iter().map(|&(m, v)| at_ms(base + m * 60_000, v)).collect()
    }

    /// The consensus reference fixtures (Danne 2017 / 2019 consensus, hand-verified):
    /// onset/recovery rules, the strict on-threshold edge, the anti-sawtooth merge, and
    /// nadir/peak capture. These pin episode detection against the literature.
    #[test]
    fn episodes_match_consensus_reference_fixtures() {
        let gap = DEFAULT_EPISODE_GAP_MS;
        // #1 clean hypo L1: onset t5, nadir 60, one event (onset t5 → recovery t25).
        let e1 = detect_episodes(
            &series(&[(0, 80.), (5, 65.), (10, 60.), (15, 62.), (20, 68.), (25, 90.), (30, 95.)]),
            70.0, true, 0, gap,
        );
        assert_eq!(e1.len(), 1);
        assert!(close(e1[0].extreme_mgdl, 60.0));
        assert!(close(e1[0].duration_min, 20.0));
        assert!(!e1[0].nocturnal);
        // #2 a 10-min dip (onset→recovery 10 min) is not an event.
        let e2 = detect_episodes(
            &series(&[(0, 80.), (5, 65.), (10, 60.), (15, 75.), (20, 80.)]),
            70.0, true, 0, gap,
        );
        assert!(e2.is_empty());
        // #3 sawtooth: the lone in-range blip (< 15-min recovery) keeps it ONE event,
        // onset t5 → recovery t40.
        let e3 = detect_episodes(
            &series(&[
                (0, 90.), (5, 60.), (10, 60.), (15, 60.), (20, 72.), (25, 60.), (30, 60.),
                (35, 60.), (40, 90.), (45, 95.), (50, 100.),
            ]),
            70.0, true, 0, gap,
        );
        assert_eq!(e3.len(), 1, "a brief recovery must not split the event");
        assert!(close(e3[0].extreme_mgdl, 60.0));
        assert!(close(e3[0].duration_min, 35.0));
        // #6 hyper L1: peak 220, onset t5 → recovery t20.
        let e6 = detect_episodes(
            &series(&[(0, 150.), (5, 200.), (10, 210.), (15, 220.), (20, 170.), (25, 160.)]),
            180.0, false, 0, gap,
        );
        assert_eq!(e6.len(), 1);
        assert!(close(e6[0].extreme_mgdl, 220.0));
        assert!(close(e6[0].duration_min, 15.0));
        // #7 a value exactly on the threshold is in-range, never in-event.
        let e7 = detect_episodes(
            &series(&[(0, 80.), (5, 70.), (10, 70.), (15, 70.), (20, 80.)]),
            70.0, true, 0, gap,
        );
        assert!(e7.is_empty(), "70 mg/dL is ≥70 — recovery, not a hypo event");
    }

    /// Level-2 (<54) events are the nested subset of the broader level-1 (<70)
    /// excursion — both are reported, with the same nadir.
    #[test]
    fn episodes_level2_nests_inside_level1() {
        let gap = DEFAULT_EPISODE_GAP_MS;
        let s = series(&[
            (0, 80.), (5, 68.), (10, 60.), (15, 50.), (20, 50.), (25, 50.), (30, 66.),
            (35, 75.), (40, 80.),
        ]);
        let l1 = detect_episodes(&s, 70.0, true, 0, gap);
        let l2 = detect_episodes(&s, 54.0, true, 0, gap);
        assert_eq!(l1.len(), 1);
        assert!(close(l1[0].extreme_mgdl, 50.0));
        assert_eq!(l2.len(), 1);
        assert!(close(l2[0].extreme_mgdl, 50.0));
        assert!(close(l2[0].duration_min, 15.0)); // onset t15 → recovery t30
    }

    /// A long sensor gap is never bridged into one continuous event — two sub-threshold
    /// runs separated by more than the episode-gap stay separate (here only the second
    /// reaches the 15-minute bar; neither merges into a single 75-minute event).
    #[test]
    fn detect_episodes_does_not_bridge_a_gap() {
        let base = 12 * 3_600_000;
        let s = series(&[(0, 60.), (5, 60.), (10, 60.), (60, 60.), (65, 60.), (70, 60.), (75, 90.)]);
        let evs = detect_episodes(&s, 70.0, true, 0, DEFAULT_EPISODE_GAP_MS);
        assert_eq!(evs.len(), 1);
        assert!(evs[0].duration_min < 30.0, "must not be one 75-min merged event");
        assert_eq!(evs[0].start_ms, base + 60 * 60_000, "the event is the second run");
    }

    /// A nocturnal low (onset 00:00–06:00 local) is flagged, honouring the local offset.
    #[test]
    fn detect_episodes_flags_nocturnal_in_local_time() {
        let base = 3 * 3_600_000; // 03:00 UTC
        let readings: Vec<_> = (0..6).map(|i| at_ms(base + i * 5 * 60_000, 60.0)).collect();
        let utc = detect_episodes(&readings, 70.0, true, 0, DEFAULT_EPISODE_GAP_MS);
        assert_eq!(utc.len(), 1);
        assert!(utc[0].nocturnal, "03:00 UTC is in the nocturnal window");
        // Shift the clock +6h: 03:00 UTC becomes 09:00 local → no longer nocturnal.
        let local = detect_episodes(&readings, 70.0, true, 6 * 60, DEFAULT_EPISODE_GAP_MS);
        assert!(!local[0].nocturnal);
    }

    /// Episode detection is unit-independent — an excursion entered in mmol/L is found
    /// exactly as its mg/dL twin would be.
    #[test]
    fn detect_episodes_is_unit_independent() {
        let base = 12 * 3_600_000;
        let mmol = |v: f64| crate::units::mgdl_to_mmol(v);
        let readings: Vec<_> = [60.0, 58.0, 62.0, 65.0, 90.0]
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                GlucoseReading::new(
                    base + i as i64 * 5 * 60_000,
                    GlucoseValue::from_mmol(mmol(v)).unwrap(),
                )
            })
            .collect();
        let evs = detect_episodes(&readings, 70.0, true, 0, DEFAULT_EPISODE_GAP_MS);
        assert_eq!(evs.len(), 1);
        assert!(close(evs[0].duration_min, 20.0)); // onset t0 → recovery t20 (the 90)
    }

    /// The AGP overlays days onto a 24-hour axis: readings at the same time-of-day on
    /// different days land in one bin, with correct percentiles; other bins stay empty.
    #[test]
    fn agp_bins_overlay_days_onto_one_axis() {
        // 08:00 on three consecutive days, values 100/110/120.
        let eight = 8 * 3_600_000;
        let readings = [
            at_ms(eight, 100.0),
            at_ms(eight + DAY_MS, 110.0),
            at_ms(eight + 2 * DAY_MS, 120.0),
        ];
        let bins = agp_bins(&readings, 15, 0);
        assert_eq!(bins.len(), 96);
        let b = &bins[32]; // 08:00 → minute 480 → bin 480/15 = 32
        assert_eq!(b.minute_of_day, 480);
        assert_eq!(b.n, 3);
        assert_eq!(b.p50, Some(110.0));
        assert_eq!(b.p25, Some(105.0));
        assert_eq!(b.p75, Some(115.0));
        // Every other bin is empty (n = 0, percentiles None) — a continuous 24h axis.
        assert_eq!(bins[0].n, 0);
        assert_eq!(bins[0].p50, None);
        assert_eq!(bins.iter().map(|b| b.n).sum::<usize>(), 3);
    }

    /// Time-of-day patterns bucket by local hour into the four standard periods.
    #[test]
    fn time_of_day_patterns_split_by_local_hour() {
        let morning = at_ms(8 * 3_600_000, 100.0); // 08:00 → morning
        let afternoon = at_ms(14 * 3_600_000, 150.0); // 14:00 → afternoon
        let p = time_of_day_patterns(&[morning, afternoon], &TirThresholds::default(), 0);
        assert_eq!(p.len(), 4);
        assert_eq!(p[0].summary.n, 0); // overnight empty
        assert_eq!(p[0].summary.mean_mgdl, None);
        assert_eq!(p[1].summary.mean_mgdl, Some(100.0)); // morning
        assert_eq!(p[2].summary.mean_mgdl, Some(150.0)); // afternoon
    }

    /// J-index combines mean and SD: at mean 100, SD 20 → 0.001·120² = 14.4.
    #[test]
    fn j_index_reference_value() {
        assert!(close(j_index(Some(100.0), Some(20.0)).unwrap(), 14.4));
        assert_eq!(j_index(None, Some(20.0)), None);
        assert_eq!(j_index(Some(100.0), None), None);
        // From readings [80, 120]: mean 100, sample SD √800 → 0.001·(100+√800)² ≈ 16.457.
        let s = GlucoseSummary::compute(&[at(0, 80.0), at(5, 120.0)], &TirThresholds::default());
        let expect = 0.001 * (100.0 + 800.0_f64.sqrt()).powi(2);
        assert!((j_index(s.mean_mgdl, s.sd_mgdl).unwrap() - expect).abs() < 1e-9);
        assert!((expect - 16.457).abs() < 0.001, "dossier J value ≈ 16.457");
    }

    /// MAGE averages the swings that exceed 1 SD. A clean ±50 oscillation (SD ≈ 27.4)
    /// has every amplitude qualify, so MAGE = 50; flat data has no qualifying swing.
    #[test]
    fn mage_averages_swings_over_one_sd() {
        let osc: Vec<_> = [100.0, 150.0, 100.0, 150.0, 100.0]
            .iter()
            .enumerate()
            .map(|(i, &v)| at(i as i64 * 5, v))
            .collect();
        assert!(close(mage(&osc).unwrap(), 50.0));
        // Flat data → SD 0 → no excursion.
        assert_eq!(mage(&[at(0, 100.0), at(5, 100.0), at(10, 100.0)]), None);
    }

    /// CONGA(1h) is the **sample** SD of the n-hour differences, matched gap-tolerantly.
    /// Series [100,130,120] at 0/60/120 min → diffs {+30, −10}, sample SD = √800 ≈ 28.284
    /// (the dossier reference value).
    #[test]
    fn conga_is_sd_of_lagged_differences() {
        let readings = [at(0, 100.0), at(60, 130.0), at(120, 120.0)];
        let c = conga(&readings, 1.0, 60_000).unwrap();
        assert!(close(c, 800.0_f64.sqrt()), "got {c}");
        // No partner within tolerance → None.
        assert_eq!(conga(&[at(0, 100.0)], 2.0, 60_000), None);
    }

    /// MODD is the mean absolute same-time-next-day difference. |130−100| and |120−130|
    /// average to 20.
    #[test]
    fn modd_is_mean_abs_daily_difference() {
        let eight = 8 * 3_600_000;
        let readings = [
            at_ms(eight, 100.0),
            at_ms(eight + DAY_MS, 130.0),
            at_ms(eight + 2 * DAY_MS, 120.0),
        ];
        let m = modd(&readings, 60_000).unwrap();
        assert!(close(m, 20.0), "got {m}");
        assert_eq!(modd(&[at_ms(eight, 100.0)], 60_000), None);
    }

    /// SD is surfaced in the summary, and CV is consistent with it (CV = SD/mean·100).
    #[test]
    fn summary_surfaces_sd_consistent_with_cv() {
        let readings = vec![at(0, 90.0), at(5, 110.0), at(10, 100.0)];
        let s = GlucoseSummary::compute(&readings, &TirThresholds::default());
        let sd = s.sd_mgdl.unwrap();
        assert!(close(sd, 10.0), "sample SD of 90/110/100 is 10"); // (100+100+0)/(3−1)
        assert!(close(s.cv_percent.unwrap(), sd / 100.0 * 100.0));
    }
}
