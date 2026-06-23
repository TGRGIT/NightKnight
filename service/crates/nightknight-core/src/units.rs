//! Glucose units — the patient-safety centerpiece of NightKnight.
//!
//! Blood-glucose concentration is reported in two units across the world:
//!
//! * **mg/dL** (milligrams per decilitre) — used in the US, and the unit CGM
//!   transmitters report natively (always an integer).
//! * **mmol/L** (millimoles per litre) — used in most of the rest of the world,
//!   conventionally shown to one decimal place.
//!
//! NightKnight treats **both units as first-class**: a value entered in mmol/L is
//! never silently coerced into "the server's unit". Instead every [`GlucoseValue`]
//! remembers the unit it was entered in (`entry_unit`) *and* carries a single
//! canonical representation in mg/dL that all maths (thresholds, time-in-range,
//! averages) is performed on. Display always honours the requested unit. This lets
//! a single data stream freely mix mg/dL and mmol/L records.
//!
//! ## The conversion constant
//!
//! Glucose (C₆H₁₂O₆) has a molar mass of **180.156 g/mol**, so:
//!
//! ```text
//! 1 mmol/L = 180.156 mg / 10 dL = 18.0156 mg/dL
//! ```
//!
//! We use [`MGDL_PER_MMOL`] = `18.0156` everywhere. This reproduces every canonical
//! clinical threshold pair exactly after rounding (e.g. 70 mg/dL ⇔ 3.9 mmol/L,
//! 180 mg/dL ⇔ 10.0 mmol/L, 54 mg/dL ⇔ 3.0 mmol/L). Legacy Nightscout used looser
//! factors (`18` or `18.0182`) in different places; see `docs/API-COMPAT.md`.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Milligrams per decilitre in one millimole per litre of glucose.
///
/// Derived from the molar mass of glucose, 180.156 g/mol. Centralised here so the
/// entire system shares one definition; pinned by the tests in this module.
pub const MGDL_PER_MMOL: f64 = 18.0156;

/// Clinical plausibility window, in mg/dL. Values outside this are almost certainly
/// a sensor error or a unit mix-up rather than a real reading, and callers should
/// treat them with suspicion (see [`GlucoseValue::is_plausible`]). The window is
/// deliberately generous — real CGMs typically report 40–400 mg/dL.
pub const PLAUSIBLE_MGDL: std::ops::RangeInclusive<f64> = 10.0..=1000.0;

/// The unit a glucose figure is expressed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GlucoseUnit {
    /// Milligrams per decilitre — integer-valued by convention.
    #[serde(rename = "mg/dl")]
    MgDl,
    /// Millimoles per litre — one-decimal-place by convention.
    #[serde(rename = "mmol/l")]
    Mmol,
}

impl GlucoseUnit {
    /// Parse the many spellings seen in the wild (Nightscout, xDrip+, Loop, …).
    ///
    /// Accepts (case-insensitive): `mg/dl`, `mgdl`, `mg`, `mg/dL` for [`MgDl`];
    /// `mmol/l`, `mmol`, `mmol/L` for [`Mmol`]. Returns `None` for anything else so
    /// callers can decide how to handle an unknown unit rather than guessing.
    ///
    /// [`MgDl`]: GlucoseUnit::MgDl
    /// [`Mmol`]: GlucoseUnit::Mmol
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mg/dl" | "mgdl" | "mg" | "mg/dl." => Some(Self::MgDl),
            "mmol/l" | "mmol" | "mmoll" => Some(Self::Mmol),
            _ => None,
        }
    }

    /// The canonical Nightscout spelling (`"mg/dl"` / `"mmol/l"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MgDl => "mg/dl",
            Self::Mmol => "mmol/l",
        }
    }

    /// Number of decimal places this unit is conventionally displayed with.
    pub fn decimals(self) -> usize {
        match self {
            Self::MgDl => 0,
            Self::Mmol => 1,
        }
    }
}

impl fmt::Display for GlucoseUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors constructing a [`GlucoseValue`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnitsError {
    /// The supplied number was NaN or infinite.
    #[error("glucose value must be a finite number")]
    NotFinite,
    /// The supplied number was negative — glucose concentration cannot be < 0.
    #[error("glucose value must not be negative")]
    Negative,
    /// A unit string could not be recognised.
    #[error("unrecognised glucose unit: {0:?}")]
    UnknownUnit(String),
}

/// A single glucose reading, unit-aware.
///
/// Internally stored as a canonical mg/dL `f64` (full precision, for maths) plus the
/// unit it was entered in (for faithful display). Construct via [`from_mgdl`],
/// [`from_mmol`], or [`new`]; read back with [`mgdl`], [`mmol`], or [`display`].
///
/// [`from_mgdl`]: GlucoseValue::from_mgdl
/// [`from_mmol`]: GlucoseValue::from_mmol
/// [`new`]: GlucoseValue::new
/// [`mgdl`]: GlucoseValue::mgdl
/// [`mmol`]: GlucoseValue::mmol
/// [`display`]: GlucoseValue::display
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlucoseValue {
    /// Canonical concentration in mg/dL, full f64 precision.
    mgdl: f64,
    /// The unit the value was originally entered/measured in.
    entry_unit: GlucoseUnit,
}

impl GlucoseValue {
    /// Construct from a value expressed in `unit`. Rejects non-finite and negative
    /// numbers (both are nonsensical for a glucose concentration).
    pub fn new(value: f64, unit: GlucoseUnit) -> Result<Self, UnitsError> {
        if !value.is_finite() {
            return Err(UnitsError::NotFinite);
        }
        if value < 0.0 {
            return Err(UnitsError::Negative);
        }
        let mgdl = match unit {
            GlucoseUnit::MgDl => value,
            GlucoseUnit::Mmol => mmol_to_mgdl(value),
        };
        Ok(Self {
            mgdl,
            entry_unit: unit,
        })
    }

    /// Construct from a value already in mg/dL.
    pub fn from_mgdl(value: f64) -> Result<Self, UnitsError> {
        Self::new(value, GlucoseUnit::MgDl)
    }

    /// Construct from a value in mmol/L.
    pub fn from_mmol(value: f64) -> Result<Self, UnitsError> {
        Self::new(value, GlucoseUnit::Mmol)
    }

    /// The unit this value was entered/measured in (its display default).
    pub fn entry_unit(self) -> GlucoseUnit {
        self.entry_unit
    }

    /// Canonical concentration in mg/dL, full precision. Use this for *maths*
    /// (comparisons, thresholds, averages) — never compare across units directly.
    pub fn mgdl(self) -> f64 {
        self.mgdl
    }

    /// Canonical concentration rounded to the nearest integer mg/dL — the form CGMs
    /// report and the form NightKnight persists as the `mgdl` column.
    pub fn mgdl_rounded(self) -> i64 {
        round_mgdl(self.mgdl) as i64
    }

    /// Concentration in mmol/L, full precision.
    pub fn mmol(self) -> f64 {
        mgdl_to_mmol(self.mgdl)
    }

    /// Raw concentration in the requested unit (no display rounding).
    pub fn value_in(self, unit: GlucoseUnit) -> f64 {
        match unit {
            GlucoseUnit::MgDl => self.mgdl,
            GlucoseUnit::Mmol => self.mmol(),
        }
    }

    /// The value rounded for **display** in the requested unit: integer mg/dL, or
    /// mmol/L to one decimal place.
    pub fn display(self, unit: GlucoseUnit) -> f64 {
        match unit {
            GlucoseUnit::MgDl => round_mgdl(self.mgdl),
            GlucoseUnit::Mmol => round_mmol(self.mmol()),
        }
    }

    /// A human-ready string in the requested unit, e.g. `"100"` or `"5.6"`.
    pub fn display_string(self, unit: GlucoseUnit) -> String {
        format!("{:.*}", unit.decimals(), self.display(unit))
    }

    /// Whether the reading falls inside the clinical plausibility window
    /// ([`PLAUSIBLE_MGDL`]). A `false` here is a strong hint of a sensor fault or a
    /// unit mix-up (e.g. a mmol/L number stored as though it were mg/dL).
    pub fn is_plausible(self) -> bool {
        PLAUSIBLE_MGDL.contains(&self.mgdl)
    }
}

/// Convert mmol/L → mg/dL. Inverse of [`mgdl_to_mmol`].
#[inline]
pub fn mmol_to_mgdl(mmol: f64) -> f64 {
    mmol * MGDL_PER_MMOL
}

/// Convert mg/dL → mmol/L. Inverse of [`mmol_to_mgdl`].
#[inline]
pub fn mgdl_to_mmol(mgdl: f64) -> f64 {
    mgdl / MGDL_PER_MMOL
}

/// Round to the nearest whole mg/dL (the display/storage convention for mg/dL).
#[inline]
pub fn round_mgdl(mgdl: f64) -> f64 {
    mgdl.round()
}

/// Round to the nearest 0.1 mmol/L (the display convention for mmol/L).
#[inline]
pub fn round_mmol(mmol: f64) -> f64 {
    (mmol * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The conversion constant must equal glucose's molar-mass-derived value. If this
    /// ever changes, every threshold and conversion in the app shifts — so we pin it.
    #[test]
    fn conversion_constant_is_molar_mass_derived() {
        assert_eq!(MGDL_PER_MMOL, 18.0156);
    }

    /// A value entered in mg/dL must come back out as exactly that integer. CGM
    /// transmitters report integer mg/dL; we must not perturb their readings.
    #[test]
    fn mgdl_native_values_are_exact() {
        for v in [40.0, 70.0, 100.0, 120.0, 180.0, 250.0, 400.0] {
            let g = GlucoseValue::from_mgdl(v).unwrap();
            assert_eq!(g.mgdl(), v);
            assert_eq!(g.mgdl_rounded(), v as i64);
            assert_eq!(g.entry_unit(), GlucoseUnit::MgDl);
        }
    }

    /// The canonical clinical threshold pairs that clinicians and the ADA/ATTD
    /// consensus use must convert exactly after display rounding. These are the
    /// numbers a person with diabetes sees every day — they have to be right.
    #[test]
    fn canonical_clinical_thresholds_round_trip() {
        // (mg/dL, mmol/L) pairs from standard time-in-range guidance.
        let pairs = [
            (54.0, 3.0),   // clinically significant ("level 2") low
            (70.0, 3.9),   // low threshold / bottom of range
            (180.0, 10.0), // high threshold / top of range
            (250.0, 13.9), // level-2 high
            (90.0, 5.0),
            (126.0, 7.0), // fasting diabetes diagnostic
        ];
        for (mgdl, mmol) in pairs {
            // mg/dL value displays as the expected mmol/L figure …
            let from_mgdl = GlucoseValue::from_mgdl(mgdl).unwrap();
            assert_eq!(
                from_mgdl.display(GlucoseUnit::Mmol),
                mmol,
                "{mgdl} mg/dL should display as {mmol} mmol/L"
            );
            // … and the mmol/L value displays as the expected mg/dL figure.
            let from_mmol = GlucoseValue::from_mmol(mmol).unwrap();
            assert_eq!(
                from_mmol.display(GlucoseUnit::MgDl),
                mgdl,
                "{mmol} mmol/L should display as {mgdl} mg/dL"
            );
        }
    }

    /// A mmol/L value remembers it was mmol/L, and displays to one decimal place.
    #[test]
    fn mmol_native_values_keep_their_unit_and_precision() {
        let g = GlucoseValue::from_mmol(5.5).unwrap();
        assert_eq!(g.entry_unit(), GlucoseUnit::Mmol);
        assert!((g.mgdl() - 99.0858).abs() < 1e-9);
        assert_eq!(g.display_string(GlucoseUnit::Mmol), "5.5");
        assert_eq!(g.display_string(GlucoseUnit::MgDl), "99");
    }

    /// Mixing units in one stream must still sort/compare correctly, because the
    /// canonical mg/dL is the single source of truth for ordering. A chart that drew
    /// a mmol/L point above a higher mg/dL point would dangerously mislead.
    #[test]
    fn mixed_unit_stream_orders_by_true_concentration() {
        let mut readings = [
            GlucoseValue::from_mmol(10.0).unwrap(), // 180 mg/dL
            GlucoseValue::from_mgdl(70.0).unwrap(),
            GlucoseValue::from_mmol(3.9).unwrap(), // ~70 mg/dL
            GlucoseValue::from_mgdl(250.0).unwrap(),
        ];
        readings.sort_by(|a, b| a.mgdl().partial_cmp(&b.mgdl()).unwrap());
        let order: Vec<i64> = readings.iter().map(|g| g.mgdl_rounded()).collect();
        // 70 and ~70 are adjacent at the bottom; 180 then 250 at the top.
        assert_eq!(order, vec![70, 70, 180, 250]);
    }

    /// Construction must reject the nonsensical values that signal corrupt data.
    #[test]
    fn rejects_non_finite_and_negative() {
        assert_eq!(
            GlucoseValue::from_mgdl(f64::NAN),
            Err(UnitsError::NotFinite)
        );
        assert_eq!(
            GlucoseValue::from_mgdl(f64::INFINITY),
            Err(UnitsError::NotFinite)
        );
        assert_eq!(GlucoseValue::from_mgdl(-1.0), Err(UnitsError::Negative));
    }

    /// Plausibility flags readings that are almost certainly a fault or unit mix-up.
    #[test]
    fn plausibility_window_flags_suspicious_readings() {
        assert!(GlucoseValue::from_mgdl(100.0).unwrap().is_plausible());
        // A mmol/L reading (5.5) mistakenly stored as mg/dL would be 5.5 mg/dL —
        // far below any survivable level, so it must read as implausible.
        assert!(!GlucoseValue::from_mgdl(5.5).unwrap().is_plausible());
        assert!(!GlucoseValue::from_mgdl(2000.0).unwrap().is_plausible());
    }

    /// Unit-string parsing covers the spellings real uploaders send.
    #[test]
    fn parses_unit_spellings() {
        for s in ["mg/dl", "mgdl", "MG/DL", " mg ", "mg/dL"] {
            assert_eq!(GlucoseUnit::parse(s), Some(GlucoseUnit::MgDl), "{s:?}");
        }
        for s in ["mmol/l", "mmol", "MMOL/L"] {
            assert_eq!(GlucoseUnit::parse(s), Some(GlucoseUnit::Mmol), "{s:?}");
        }
        assert_eq!(GlucoseUnit::parse("furlongs"), None);
    }

    proptest! {
        /// PROPERTY: a value entered in mg/dL always reads back as that mg/dL figure.
        /// No conversion noise may creep into native mg/dL data.
        #[test]
        fn prop_mgdl_is_lossless(v in 0.0f64..=1000.0) {
            let g = GlucoseValue::from_mgdl(v).unwrap();
            prop_assert_eq!(g.mgdl(), v);
        }

        /// PROPERTY: converting mg/dL → mmol/L → mg/dL stays within ½ mg/dL, i.e. the
        /// conversion never moves a reading by a clinically meaningful amount.
        #[test]
        fn prop_round_trip_within_half_mgdl(v in 10.0f64..=600.0) {
            let back = mmol_to_mgdl(mgdl_to_mmol(v));
            prop_assert!((back - v).abs() < 0.5, "round-trip drift too large: {} -> {}", v, back);
        }

        /// PROPERTY: ordering is preserved across units. If one reading is higher in
        /// mg/dL it must also be higher (or equal) in mmol/L — monotonicity is what
        /// makes a mixed-unit chart trustworthy.
        #[test]
        fn prop_conversion_is_monotonic(a in 0.0f64..=1000.0, b in 0.0f64..=1000.0) {
            let ma = mgdl_to_mmol(a);
            let mb = mgdl_to_mmol(b);
            prop_assert_eq!(a.partial_cmp(&b), ma.partial_cmp(&mb));
        }

        /// PROPERTY: mmol/L display rounding always lands on a 0.1 grid point.
        #[test]
        fn prop_mmol_display_is_one_decimal(v in 0.0f64..=1000.0) {
            let shown = round_mmol(mgdl_to_mmol(v));
            // shown * 10 must be (within fp tolerance) a whole number.
            let tenths = shown * 10.0;
            prop_assert!((tenths - tenths.round()).abs() < 1e-6);
        }
    }
}
