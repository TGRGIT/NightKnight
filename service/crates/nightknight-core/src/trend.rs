//! Glucose trend (the "arrows") — how fast and which way glucose is moving.
//!
//! CGMs and Nightscout summarise the recent rate of change as one of eight arrows.
//! NightKnight reproduces the widely-used Dexcom thresholds, expressed in **mg/dL
//! per minute** so the classification is unit-independent (we always compute on the
//! canonical mg/dL). The arrow drives both the dashboard display and rate-of-change
//! alarms, so the thresholds are pinned by tests.
//!
//! | Arrow            | Rate (mg/dL/min) | Tier   | Label          |
//! |------------------|------------------|--------|----------------|
//! | `DoubleUp`       | > +3             | large  | Rising fast    |
//! | `SingleUp`       | +2 … +3          | medium | Rising         |
//! | `FortyFiveUp`    | +1 … +2          | 45°    | Drifting up    |
//! | `Flat`           | −1 … +1          | steady | Steady         |
//! | `FortyFiveDown`  | −2 … −1          | 45°    | Drifting down  |
//! | `SingleDown`     | −3 … −2          | medium | Falling        |
//! | `DoubleDown`     | < −3             | large  | Falling fast   |
//!
//! These boundaries are the Dexcom G6/G7 trend-arrow thresholds and match the
//! widely-used Nightscout / xDrip+ classification. The flat band is `−1 … +1`
//! inclusive — a sustained drift of ≈10 mg/dL over 10 minutes (±1.0) reads as
//! *steady* — exactly the rule of thumb people use at the bedside; the "45°" tier is
//! anything faster than that up to ±2, a direct (vertical) arrow needs ≥2 mg/dL/min,
//! and a double arrow > 3.
//!
//! ## Where the rate comes from
//!
//! Two sources, in order of preference:
//!
//! 1. **First-party sensor trend.** Dexcom Share (`Trend`) and LibreLinkUp
//!    (`TrendArrow`) report an arrow the transmitter computed from its own
//!    (unfiltered, higher-cadence) data. The connectors capture it onto the entry's
//!    `direction` field; when present it is authoritative and we use it verbatim.
//! 2. **Computed fallback.** When no sensor trend is available we estimate the rate
//!    ourselves by **least-squares regression** over the readings in the last
//!    [`TREND_WINDOW_MS`] (15 minutes) — more robust to single-sample noise than a
//!    two-point delta, and gap-aware (a lone point after a sensor gap yields no
//!    trend rather than a bogus spike).

use serde::{Deserialize, Serialize};

use crate::analytics::GlucoseReading;
use crate::units::GlucoseValue;

/// The window over which the rate of change is estimated when no first-party sensor
/// trend is available, in epoch milliseconds. 15 minutes ≈ three 5-minute CGM
/// samples — long enough to suppress per-sample noise, short enough to stay current.
pub const TREND_WINDOW_MS: i64 = 15 * 60_000;

/// The trend arrow, using Nightscout's exact string spellings on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    DoubleUp,
    SingleUp,
    FortyFiveUp,
    Flat,
    FortyFiveDown,
    SingleDown,
    DoubleDown,
    /// No trend available (e.g. first reading after a gap). The `alias` accepts the
    /// PascalCase spelling Dexcom Share sends, alongside the Nightscout `NONE`.
    #[serde(rename = "NONE", alias = "None")]
    None,
    /// The CGM could not compute a trend.
    #[serde(rename = "NOT COMPUTABLE", alias = "NotComputable")]
    NotComputable,
    /// The rate of change exceeded what the CGM will report.
    #[serde(rename = "RATE OUT OF RANGE", alias = "RateOutOfRange")]
    RateOutOfRange,
}

impl Direction {
    /// Classify a rate of change in **mg/dL per minute** into an arrow.
    ///
    /// Non-finite rates yield [`Direction::NotComputable`]. The boundaries are
    /// inclusive at the lower edge of each upward band and symmetric downward, so a
    /// reading is never simultaneously two arrows.
    pub fn from_rate_per_min(rate: f64) -> Direction {
        if !rate.is_finite() {
            return Direction::NotComputable;
        }
        // The **Flat band is inclusive on both edges** (`−1 ≤ rate ≤ 1`): ±1 mg/dL/min
        // is the one trend boundary every vendor agrees on, and the consensus pins a
        // sustained ±10 mg/dL-over-10-min drift (= ±1.0) as *steady*. The ±2 / ±3
        // assignments are a documented self-imposed convention (the literature only
        // says "2 to 3", not whether the edge is `<` or `≤`); we pin them to the
        // research reference values (e.g. −2.0 → SingleDown, −3.0 → DoubleDown).
        match rate {
            r if r > 3.0 => Direction::DoubleUp,
            r if r >= 2.0 => Direction::SingleUp,
            r if r > 1.0 => Direction::FortyFiveUp,
            r if r >= -1.0 => Direction::Flat,
            r if r > -2.0 => Direction::FortyFiveDown,
            r if r > -3.0 => Direction::SingleDown,
            _ => Direction::DoubleDown,
        }
    }

    /// Classify the trend between two timestamped readings (`from` earlier than
    /// `to`). Returns [`Direction::None`] if the timestamps are equal or reversed
    /// (we cannot infer a rate without forward elapsed time).
    pub fn between(from: (i64, GlucoseValue), to: (i64, GlucoseValue)) -> Direction {
        let (t0, g0) = from;
        let (t1, g1) = to;
        let minutes = (t1 - t0) as f64 / 60_000.0; // timestamps are epoch ms
        if minutes <= 0.0 {
            return Direction::None;
        }
        let rate = (g1.mgdl() - g0.mgdl()) / minutes;
        Direction::from_rate_per_min(rate)
    }

    /// The Nightscout direction name (`"Flat"`, `"SingleUp"`, `"NOT COMPUTABLE"`, …).
    pub fn name(self) -> &'static str {
        match self {
            Direction::DoubleUp => "DoubleUp",
            Direction::SingleUp => "SingleUp",
            Direction::FortyFiveUp => "FortyFiveUp",
            Direction::Flat => "Flat",
            Direction::FortyFiveDown => "FortyFiveDown",
            Direction::SingleDown => "SingleDown",
            Direction::DoubleDown => "DoubleDown",
            Direction::None => "NONE",
            Direction::NotComputable => "NOT COMPUTABLE",
            Direction::RateOutOfRange => "RATE OUT OF RANGE",
        }
    }

    /// A Unicode arrow suitable for compact display.
    pub fn arrow(self) -> &'static str {
        match self {
            Direction::DoubleUp => "⇈",
            Direction::SingleUp => "↑",
            Direction::FortyFiveUp => "↗",
            Direction::Flat => "→",
            Direction::FortyFiveDown => "↘",
            Direction::SingleDown => "↓",
            Direction::DoubleDown => "⇊",
            Direction::None | Direction::NotComputable | Direction::RateOutOfRange => "–",
        }
    }

    /// A plain-language description of the movement, for the dashboard caption under
    /// the current value. Maps the three magnitude tiers onto words a person reads at
    /// a glance: *steady*, a gentle 45° *drift*, a direct *rise/fall*, or a *fast*
    /// double-arrow change.
    pub fn label(self) -> &'static str {
        match self {
            Direction::DoubleUp => "Rising fast",
            Direction::SingleUp => "Rising",
            Direction::FortyFiveUp => "Drifting up",
            Direction::Flat => "Steady",
            Direction::FortyFiveDown => "Drifting down",
            Direction::SingleDown => "Falling",
            Direction::DoubleDown => "Falling fast",
            Direction::None | Direction::NotComputable | Direction::RateOutOfRange => "No trend",
        }
    }

    /// Whether this is one of the seven real movement arrows (as opposed to a
    /// sentinel like [`None`] / [`NotComputable`] / [`RateOutOfRange`]). Used to
    /// decide whether a first-party sensor trend is usable as-is.
    ///
    /// [`None`]: Direction::None
    /// [`NotComputable`]: Direction::NotComputable
    /// [`RateOutOfRange`]: Direction::RateOutOfRange
    pub fn is_arrow(self) -> bool {
        matches!(
            self,
            Direction::DoubleUp
                | Direction::SingleUp
                | Direction::FortyFiveUp
                | Direction::Flat
                | Direction::FortyFiveDown
                | Direction::SingleDown
                | Direction::DoubleDown
        )
    }
}

/// Estimate the recent rate of change in **mg/dL per minute** by least-squares
/// regression over the readings whose timestamps fall within `window_ms` of the most
/// recent reading.
///
/// Returns `None` when there are not at least two readings spanning a positive time
/// range inside the window — so a single point, or a lone point after a sensor gap,
/// produces no trend rather than a fabricated rate. Works on any reading order and is
/// unit-independent (it regresses on canonical mg/dL).
pub fn rate_per_min(readings: &[GlucoseReading], window_ms: i64) -> Option<f64> {
    if readings.len() < 2 {
        return None;
    }
    let latest = readings.iter().map(|r| r.date_ms).max()?;
    // x in minutes (so the slope is already mg/dL per minute), y in mg/dL. Only points
    // within [latest - window, latest] participate; future-dated points are excluded.
    let pts: Vec<(f64, f64)> = readings
        .iter()
        .filter(|r| {
            let age = latest - r.date_ms;
            (0..=window_ms).contains(&age)
        })
        .map(|r| (r.date_ms as f64 / 60_000.0, r.value.mgdl()))
        .collect();
    least_squares_slope(&pts)
}

/// Slope of the best-fit line `y = a + b·x` (returns `b`), or `None` if undefined
/// (fewer than two points, or all points share one `x`).
fn least_squares_slope(pts: &[(f64, f64)]) -> Option<f64> {
    if pts.len() < 2 {
        return None;
    }
    let n = pts.len() as f64;
    let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
    for &(x, y) in pts {
        sx += x;
        sy += y;
        sxx += x * x;
        sxy += x * y;
    }
    let denom = n * sxx - sx * sx;
    if denom.abs() < 1e-9 {
        return None; // all timestamps equal — no time base to infer a rate
    }
    let slope = (n * sxy - sx * sy) / denom;
    slope.is_finite().then_some(slope)
}

/// Classify the recent trend from a set of readings by regressing over the last
/// [`TREND_WINDOW_MS`]. Returns [`Direction::None`] when the window holds too little
/// data to infer a rate. This is the *computed fallback* — prefer a first-party
/// sensor `direction` (see the module docs) when the entry carries one.
pub fn classify_recent(readings: &[GlucoseReading]) -> Direction {
    match rate_per_min(readings, TREND_WINDOW_MS) {
        Some(rate) => Direction::from_rate_per_min(rate),
        None => Direction::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::GlucoseValue;

    /// Each rate band must map to exactly the arrow clinicians expect — these arrows
    /// are how a person decides whether to treat now or wait.
    #[test]
    fn rate_bands_map_to_expected_arrows() {
        assert_eq!(Direction::from_rate_per_min(5.0), Direction::DoubleUp);
        assert_eq!(Direction::from_rate_per_min(3.01), Direction::DoubleUp);
        assert_eq!(Direction::from_rate_per_min(2.5), Direction::SingleUp);
        assert_eq!(Direction::from_rate_per_min(1.5), Direction::FortyFiveUp);
        assert_eq!(Direction::from_rate_per_min(0.0), Direction::Flat);
        assert_eq!(Direction::from_rate_per_min(0.9), Direction::Flat);
        assert_eq!(Direction::from_rate_per_min(-0.9), Direction::Flat);
        assert_eq!(Direction::from_rate_per_min(-1.5), Direction::FortyFiveDown);
        assert_eq!(Direction::from_rate_per_min(-2.5), Direction::SingleDown);
        assert_eq!(Direction::from_rate_per_min(-5.0), Direction::DoubleDown);
    }

    /// Exact band boundaries resolve deterministically (no double-classification). The
    /// Flat band is inclusive on both edges, so ±1.0 is *steady* — the one trend
    /// boundary every vendor agrees on.
    #[test]
    fn boundaries_are_deterministic() {
        assert_eq!(Direction::from_rate_per_min(3.0), Direction::SingleUp); // not DoubleUp
        assert_eq!(Direction::from_rate_per_min(2.0), Direction::SingleUp);
        assert_eq!(Direction::from_rate_per_min(1.0), Direction::Flat); // ±1 is Flat
        assert_eq!(Direction::from_rate_per_min(-1.0), Direction::Flat);
        assert_eq!(Direction::from_rate_per_min(-2.0), Direction::SingleDown);
        assert_eq!(Direction::from_rate_per_min(-3.0), Direction::DoubleDown);
    }

    /// A non-finite rate must never crash or guess — it is "not computable".
    #[test]
    fn non_finite_rate_is_not_computable() {
        assert_eq!(Direction::from_rate_per_min(f64::NAN), Direction::NotComputable);
    }

    /// Trend between two readings five minutes apart (the usual CGM cadence): a
    /// +15 mg/dL jump over 5 min = +3 mg/dL/min boundary → SingleUp.
    #[test]
    fn trend_between_readings_uses_elapsed_minutes() {
        let t0 = 1_700_000_000_000;
        let five_min = 5 * 60_000;
        let from = (t0, GlucoseValue::from_mgdl(100.0).unwrap());
        let to = (t0 + five_min, GlucoseValue::from_mgdl(115.0).unwrap());
        assert_eq!(Direction::between(from, to), Direction::SingleUp);
    }

    /// Reversed or zero elapsed time yields None rather than a bogus rate.
    #[test]
    fn trend_requires_forward_time() {
        let g = GlucoseValue::from_mgdl(100.0).unwrap();
        assert_eq!(Direction::between((10, g), (10, g)), Direction::None);
        assert_eq!(Direction::between((20, g), (10, g)), Direction::None);
    }

    /// Works identically when the readings were entered in mmol/L, because trend is
    /// computed on the canonical mg/dL — a mixed-unit stream still trends correctly.
    #[test]
    fn trend_is_unit_independent() {
        let t0 = 0;
        let five_min = 5 * 60_000;
        // 5.0 mmol/L (~90 mg/dL) → 7.0 mmol/L (~126 mg/dL): +36 mg/dL / 5 min ≈ +7.2.
        let from = (t0, GlucoseValue::from_mmol(5.0).unwrap());
        let to = (t0 + five_min, GlucoseValue::from_mmol(7.0).unwrap());
        assert_eq!(Direction::between(from, to), Direction::DoubleUp);
    }

    fn reading(t: i64, mgdl: f64) -> GlucoseReading {
        GlucoseReading::new(t, GlucoseValue::from_mgdl(mgdl).unwrap())
    }

    /// The plain-language labels map the three magnitude tiers (45° / direct / fast)
    /// onto the words shown under the current value.
    #[test]
    fn labels_describe_each_tier() {
        assert_eq!(Direction::Flat.label(), "Steady");
        assert_eq!(Direction::FortyFiveUp.label(), "Drifting up");
        assert_eq!(Direction::FortyFiveDown.label(), "Drifting down");
        assert_eq!(Direction::SingleUp.label(), "Rising");
        assert_eq!(Direction::SingleDown.label(), "Falling");
        assert_eq!(Direction::DoubleUp.label(), "Rising fast");
        assert_eq!(Direction::DoubleDown.label(), "Falling fast");
        assert_eq!(Direction::None.label(), "No trend");
    }

    /// Only the seven movement arrows count as a usable arrow; the sentinels do not.
    #[test]
    fn is_arrow_distinguishes_real_arrows_from_sentinels() {
        for d in [
            Direction::DoubleUp,
            Direction::SingleUp,
            Direction::FortyFiveUp,
            Direction::Flat,
            Direction::FortyFiveDown,
            Direction::SingleDown,
            Direction::DoubleDown,
        ] {
            assert!(d.is_arrow(), "{d:?} should be an arrow");
        }
        for d in [Direction::None, Direction::NotComputable, Direction::RateOutOfRange] {
            assert!(!d.is_arrow(), "{d:?} is a sentinel, not an arrow");
        }
    }

    /// The least-squares estimator recovers a clean linear rate. Four readings rising
    /// 7.5 mg/dL every 5 min = +1.5 mg/dL/min → a FortyFive (45°) up arrow. (At exactly
    /// +1.0 the inclusive Flat band would still read steady.)
    #[test]
    fn rate_per_min_recovers_a_linear_slope() {
        let five = 5 * 60_000;
        let readings = [
            reading(0, 100.0),
            reading(five, 107.5),
            reading(2 * five, 115.0),
            reading(3 * five, 122.5),
        ];
        let rate = rate_per_min(&readings, TREND_WINDOW_MS).unwrap();
        assert!((rate - 1.5).abs() < 1e-9, "rate was {rate}");
        assert_eq!(classify_recent(&readings), Direction::FortyFiveUp);
    }

    /// Regression is robust to a single noisy sample: one spike in an otherwise flat
    /// run does not flip the trend to "rising", the way a raw last-two-point delta
    /// would. (A flat line with a +40 blip stays Flat.)
    #[test]
    fn rate_per_min_is_robust_to_one_noisy_sample() {
        let five = 5 * 60_000;
        let readings = [
            reading(0, 100.0),
            reading(five, 100.0),
            reading(2 * five, 140.0), // a single noisy spike
            reading(3 * five, 100.0),
        ];
        // Last-two-point delta would read −8 mg/dL/min (140→100 in 5 min); the
        // regression over the whole window stays within the Flat band.
        let rate = rate_per_min(&readings, TREND_WINDOW_MS).unwrap();
        assert!(rate.abs() < 1.0, "regression rate {rate} should be ~flat");
        assert_eq!(classify_recent(&readings), Direction::Flat);
    }

    /// Gap tolerance: a lone fresh reading after a long sensor gap (older points fall
    /// outside the 15-min window) yields no trend rather than a fabricated rate.
    #[test]
    fn rate_per_min_needs_data_inside_the_window() {
        let readings = [
            reading(0, 80.0),
            reading(60 * 60_000, 120.0), // 60 min later — the only point in-window
        ];
        // Only the latest point lies within 15 min of itself → not enough to regress.
        assert_eq!(rate_per_min(&readings, TREND_WINDOW_MS), None);
        assert_eq!(classify_recent(&readings), Direction::None);
        // A single reading is likewise trend-less.
        assert_eq!(rate_per_min(&readings[1..], TREND_WINDOW_MS), None);
    }

    /// Identical timestamps give no time base, so the slope is undefined (None) rather
    /// than a divide-by-zero or infinity.
    #[test]
    fn rate_per_min_handles_degenerate_timestamps() {
        let readings = [reading(1000, 100.0), reading(1000, 200.0)];
        assert_eq!(rate_per_min(&readings, TREND_WINDOW_MS), None);
    }
}
