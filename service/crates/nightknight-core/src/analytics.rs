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

impl GlucoseBand {
    /// A plain-language status label for the level, matching the alert vocabulary the
    /// CGM ecosystem uses (Dexcom's "Urgent Low" at the level-2 threshold, plus Low /
    /// In range / High / Urgent high). This is the glucose **level** dimension, which
    /// is distinct from the **trend** (see [`crate::trend::Direction::label`]): a
    /// reading can be "Low" and "Rising" at the same time.
    pub fn label(self) -> &'static str {
        match self {
            GlucoseBand::VeryLow => "Urgent low",
            GlucoseBand::Low => "Low",
            GlucoseBand::InRange => "In range",
            GlucoseBand::High => "High",
            GlucoseBand::VeryHigh => "Urgent high",
        }
    }

    /// A short machine token for the level (`veryLow` … `veryHigh`), for clients that
    /// want to drive styling/colour off the band rather than re-deriving thresholds.
    pub fn key(self) -> &'static str {
        match self {
            GlucoseBand::VeryLow => "veryLow",
            GlucoseBand::Low => "low",
            GlucoseBand::InRange => "inRange",
            GlucoseBand::High => "high",
            GlucoseBand::VeryHigh => "veryHigh",
        }
    }
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

/// Population standard deviation of glucose in mg/dL, or `None` if fewer than two
/// readings (variability is undefined for a single point).
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
        / readings.len() as f64;
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
    pub gmi_percent: Option<f64>,
    pub estimated_a1c_percent: Option<f64>,
    pub cv_percent: Option<f64>,
    pub tir: TimeInRange,
}

impl GlucoseSummary {
    /// Compute every metric in one pass-friendly call.
    pub fn compute(readings: &[GlucoseReading], thresholds: &TirThresholds) -> GlucoseSummary {
        let mean = mean_mgdl(readings);
        GlucoseSummary {
            n: readings.len(),
            mean_mgdl: mean,
            gmi_percent: mean.map(gmi_percent),
            estimated_a1c_percent: mean.map(estimated_a1c_percent),
            cv_percent: coefficient_of_variation(readings),
            tir: TimeInRange::compute(readings, thresholds),
        }
    }
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

    /// Level labels match the CGM-ecosystem status vocabulary the user expects.
    #[test]
    fn band_labels_match_status_vocabulary() {
        let t = TirThresholds::default();
        assert_eq!(t.band(40.0).label(), "Urgent low");
        assert_eq!(t.band(60.0).label(), "Low");
        assert_eq!(t.band(120.0).label(), "In range");
        assert_eq!(t.band(220.0).label(), "High");
        assert_eq!(t.band(300.0).label(), "Urgent high");
        assert_eq!(t.band(300.0).key(), "veryHigh");
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
    }

    /// Mean, SD and CV on a tiny known set.
    #[test]
    fn mean_sd_cv_on_known_set() {
        // values 90, 110, 100 → mean 100; population variance = (100+100+0)/3 = 66.67
        let readings = vec![mgdl(90.0), mgdl(110.0), mgdl(100.0)];
        assert_eq!(mean_mgdl(&readings), Some(100.0));
        let sd = std_dev_mgdl(&readings).unwrap();
        assert!((sd - (200.0f64 / 3.0).sqrt()).abs() < 1e-9);
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
}
