//! Glucose trend (the "arrows") — how fast and which way glucose is moving.
//!
//! CGMs and Nightscout summarise the recent rate of change as one of eight arrows.
//! NightKnight reproduces the widely-used Dexcom thresholds, expressed in **mg/dL
//! per minute** so the classification is unit-independent (we always compute on the
//! canonical mg/dL). The arrow drives both the dashboard display and rate-of-change
//! alarms, so the thresholds are pinned by tests.
//!
//! | Arrow            | Rate (mg/dL/min) |
//! |------------------|------------------|
//! | `DoubleUp`       | > +3             |
//! | `SingleUp`       | +2 … +3          |
//! | `FortyFiveUp`    | +1 … +2          |
//! | `Flat`           | −1 … +1          |
//! | `FortyFiveDown`  | −2 … −1          |
//! | `SingleDown`     | −3 … −2          |
//! | `DoubleDown`     | < −3             |

use serde::{Deserialize, Serialize};

use crate::units::GlucoseValue;

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
    /// No trend available (e.g. first reading after a gap).
    #[serde(rename = "NONE")]
    None,
    /// The CGM could not compute a trend.
    #[serde(rename = "NOT COMPUTABLE")]
    NotComputable,
    /// The rate of change exceeded what the CGM will report.
    #[serde(rename = "RATE OUT OF RANGE")]
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
        match rate {
            r if r > 3.0 => Direction::DoubleUp,
            r if r >= 2.0 => Direction::SingleUp,
            r if r >= 1.0 => Direction::FortyFiveUp,
            r if r > -1.0 => Direction::Flat,
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

    /// Exact band boundaries resolve deterministically (no double-classification).
    #[test]
    fn boundaries_are_deterministic() {
        assert_eq!(Direction::from_rate_per_min(3.0), Direction::SingleUp); // not DoubleUp
        assert_eq!(Direction::from_rate_per_min(2.0), Direction::SingleUp);
        assert_eq!(Direction::from_rate_per_min(1.0), Direction::FortyFiveUp);
        assert_eq!(Direction::from_rate_per_min(-1.0), Direction::FortyFiveDown);
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
}
