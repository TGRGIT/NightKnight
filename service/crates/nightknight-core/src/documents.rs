//! Nightscout document types — `entries`, `treatments`, `devicestatus`, `profile`.
//!
//! These mirror the Nightscout data model so the v1/v3 compatibility layers can read
//! and write them verbatim. Two design choices matter for safety and compatibility:
//!
//! 1. **Unknown fields are preserved.** Real uploaders attach app-specific fields we
//!    don't model; every type keeps an `extra` map (`#[serde(flatten)]`) so nothing
//!    is silently dropped on round-trip.
//! 2. **Loosely-typed where the wire is loose.** Fields like `direction` are kept as
//!    `String` (not the [`Direction`] enum) so an unfamiliar value never causes a
//!    whole upload to be rejected; callers parse them when needed.
//!
//! Clinical validation lives on each type ([`Entry::validate`] etc.) and is the
//! gate that keeps impossible or dangerous values out of the database.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::timeutil::normalize_epoch_ms;
use crate::trend::Direction;
use crate::units::{GlucoseUnit, GlucoseValue};

/// Earliest plausible record time (2000-01-01T00:00:00Z) in epoch ms. Anything older
/// is almost certainly a clock bug or a wrong-unit timestamp (e.g. seconds as ms).
pub const MIN_PLAUSIBLE_MS: i64 = 946_684_800_000;

/// How far into the future a record may be timestamped before we reject it (clock
/// skew tolerance). 24 hours.
pub const FUTURE_TOLERANCE_MS: i64 = 24 * 60 * 60 * 1000;

/// A clinical/structural validation failure. These are rejections of data that must
/// never reach storage, each with a human-readable reason.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DocumentError {
    #[error("{field} must be present")]
    Missing { field: &'static str },
    #[error("{field} must be a finite, non-negative number (got {value})")]
    NotNonNegative { field: &'static str, value: f64 },
    #[error("timestamp {0} ms is implausible (before 2000 or too far in the future)")]
    ImplausibleTimestamp(i64),
    #[error("glucose reading is implausible: {0}")]
    ImplausibleGlucose(String),
    #[error("unrecognised glucose unit: {0:?}")]
    UnknownUnit(String),
}

fn check_non_negative(field: &'static str, v: Option<f64>) -> Result<(), DocumentError> {
    if let Some(x) = v {
        if !x.is_finite() || x < 0.0 {
            return Err(DocumentError::NotNonNegative { field, value: x });
        }
    }
    Ok(())
}

fn check_timestamp(ms: i64, now_ms: i64) -> Result<(), DocumentError> {
    if ms < MIN_PLAUSIBLE_MS || ms > now_ms + FUTURE_TOLERANCE_MS {
        return Err(DocumentError::ImplausibleTimestamp(ms));
    }
    Ok(())
}

/// A glucose/CGM entry (`sgv`, `mbg`, `cal`, …).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// Record kind: `"sgv"` (sensor glucose), `"mbg"` (meter/finger), `"cal"`, etc.
    #[serde(rename = "type")]
    pub entry_type: String,
    /// Time of the reading, epoch milliseconds.
    pub date: i64,
    /// ISO-8601 form of `date` (some clients send/expect it).
    #[serde(rename = "dateString", default, skip_serializing_if = "Option::is_none")]
    pub date_string: Option<String>,
    /// Sensor glucose value, in the unit named by `units` (defaults to mg/dL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sgv: Option<f64>,
    /// Trend arrow as the raw wire string (parse via [`Entry::direction_parsed`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noise: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filtered: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unfiltered: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rssi: Option<i64>,
    /// The unit `sgv` is expressed in (`"mg/dl"` / `"mmol/l"`); defaults to mg/dL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<String>,
    /// Any other fields the producing app attached, preserved on round-trip.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Entry {
    /// The unit this entry's glucose is in (mg/dL if unspecified).
    pub fn unit(&self) -> Result<GlucoseUnit, DocumentError> {
        match &self.units {
            None => Ok(GlucoseUnit::MgDl),
            Some(s) => GlucoseUnit::parse(s).ok_or_else(|| DocumentError::UnknownUnit(s.clone())),
        }
    }

    /// The glucose reading as a unit-aware [`GlucoseValue`], if this entry carries one.
    pub fn glucose_value(&self) -> Result<Option<GlucoseValue>, DocumentError> {
        match self.sgv {
            None => Ok(None),
            Some(v) => {
                let g = GlucoseValue::new(v, self.unit()?)
                    .map_err(|e| DocumentError::ImplausibleGlucose(e.to_string()))?;
                Ok(Some(g))
            }
        }
    }

    /// Parse the trend string into a typed [`Direction`], if recognised.
    pub fn direction_parsed(&self) -> Option<Direction> {
        self.direction
            .as_ref()
            .and_then(|d| serde_json::from_value(Value::String(d.clone())).ok())
    }

    /// Validate the entry against clinical and structural rules. `now_ms` is the
    /// current time (passed in because core does no I/O / clock access).
    pub fn validate(&self, now_ms: i64) -> Result<(), DocumentError> {
        // A `date` sent in seconds (a common uploader bug) is rescaled to ms before the
        // plausibility check, matching how storage derives `mills` — otherwise a valid
        // reading would be dropped here yet stored at the right time. The year-2000
        // floor still rejects genuinely garbled or pre-2000 timestamps after rescaling.
        check_timestamp(normalize_epoch_ms(self.date), now_ms)?;
        // An SGV/MBG record must carry a plausible glucose value.
        if matches!(self.entry_type.as_str(), "sgv" | "mbg") {
            let g = self
                .glucose_value()?
                .ok_or(DocumentError::Missing { field: "sgv" })?;
            if !g.is_plausible() {
                return Err(DocumentError::ImplausibleGlucose(format!(
                    "{} mg/dL outside plausible range",
                    g.mgdl_rounded()
                )));
            }
        }
        Ok(())
    }
}

/// A treatment / event (bolus, carbs, BG check, temp basal, note, …).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Treatment {
    /// e.g. `"Meal Bolus"`, `"Correction Bolus"`, `"BG Check"`, `"Temp Basal"`.
    #[serde(rename = "eventType")]
    pub event_type: String,
    /// ISO-8601 timestamp (the canonical treatment time in Nightscout).
    #[serde(rename = "created_at", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Epoch-ms timestamp (some clients send `date`/`mills`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<i64>,
    /// Glucose at the time of the event, in `units`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glucose: Option<f64>,
    #[serde(rename = "glucoseType", default, skip_serializing_if = "Option::is_none")]
    pub glucose_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carbs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protein: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fat: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insulin: Option<f64>,
    /// Duration in minutes (temp basals, temp targets, exercise, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(rename = "enteredBy", default, skip_serializing_if = "Option::is_none")]
    pub entered_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Treatment {
    /// Validate the treatment. Nutrition and dose amounts must be finite and
    /// non-negative — a negative bolus or carb count is nonsensical and dangerous if
    /// it feeds into insulin-on-board maths downstream.
    pub fn validate(&self) -> Result<(), DocumentError> {
        check_non_negative("carbs", self.carbs)?;
        check_non_negative("protein", self.protein)?;
        check_non_negative("fat", self.fat)?;
        check_non_negative("insulin", self.insulin)?;
        check_non_negative("duration", self.duration)?;
        if self.event_type.trim().is_empty() {
            return Err(DocumentError::Missing { field: "eventType" });
        }
        Ok(())
    }
}

/// Device status (pump/uploader battery, loop status, IOB, …). Mostly free-form, so
/// we keep a couple of typed fields and preserve the rest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeviceStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(rename = "created_at", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A treatment/insulin profile (basal schedule, ISF, carb ratio, targets). The
/// nested `store` of named profiles is kept as free-form JSON in `extra`; the typed
/// fields are the ones the dashboard needs directly.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    /// Default display unit for the profile (`"mg/dl"` / `"mmol/l"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<String>,
    #[serde(rename = "defaultProfile", default, skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
    #[serde(rename = "created_at", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trend::Direction;

    const NOW: i64 = 1_700_000_000_000; // 2023-11-14, a fixed "now" for tests.

    /// A typical xDrip+/Dexcom SGV upload deserialises, keeps unknown fields, and
    /// validates. This is the single most common write the system handles.
    #[test]
    fn parses_and_validates_a_real_sgv_entry() {
        let json = r#"{
            "type": "sgv",
            "date": 1699999999000,
            "dateString": "2023-11-14T22:13:19.000Z",
            "sgv": 112,
            "direction": "Flat",
            "device": "xDrip-DexcomG6",
            "noise": 1,
            "filtered": 113000,
            "unfiltered": 112500,
            "rssi": 100,
            "appSpecificField": "keep me"
        }"#;
        let e: Entry = serde_json::from_str(json).unwrap();
        assert_eq!(e.entry_type, "sgv");
        assert_eq!(e.sgv, Some(112.0));
        assert_eq!(e.direction_parsed(), Some(Direction::Flat));
        assert_eq!(e.glucose_value().unwrap().unwrap().mgdl_rounded(), 112);
        e.validate(NOW).unwrap();
        // Unknown field survived …
        assert_eq!(e.extra.get("appSpecificField").unwrap(), "keep me");
        // … and is re-emitted on serialize.
        let out = serde_json::to_string(&e).unwrap();
        assert!(out.contains("appSpecificField"));
    }

    /// A mmol/L meter entry is accepted and its value is interpreted in mmol/L.
    #[test]
    fn parses_mmol_meter_entry() {
        let json = r#"{ "type": "mbg", "date": 1699999999000, "mbg": 99, "sgv": 5.5, "units": "mmol" }"#;
        let e: Entry = serde_json::from_str(json).unwrap();
        assert_eq!(e.unit().unwrap(), GlucoseUnit::Mmol);
        assert_eq!(e.glucose_value().unwrap().unwrap().mgdl_rounded(), 99);
        e.validate(NOW).unwrap();
    }

    /// An SGV entry with no glucose value is rejected — we must not store a "reading"
    /// that has no reading.
    #[test]
    fn sgv_without_value_is_rejected() {
        let e = Entry {
            entry_type: "sgv".into(),
            date: NOW - 1000,
            date_string: None,
            sgv: None,
            direction: None,
            device: None,
            noise: None,
            filtered: None,
            unfiltered: None,
            rssi: None,
            units: None,
            extra: Map::new(),
        };
        assert_eq!(e.validate(NOW), Err(DocumentError::Missing { field: "sgv" }));
    }

    /// A 10-digit `date` in *seconds* (a classic uploader bug) is rescaled to ms and
    /// accepted at the right time — matching how storage derives `mills`, so a valid
    /// reading is no longer dropped here only to be stored elsewhere. Genuinely garbage
    /// timestamps still fail: a value too small to be seconds-since-epoch, and a real
    /// pre-2000 millisecond clock bug, both stay rejected by the year-2000 floor.
    #[test]
    fn seconds_timestamp_is_rescaled_then_validated() {
        // 1_699_999_999 s → 1_699_999_999_000 ms (2023-11-14), within range → accepted.
        let mut e = sample_sgv();
        e.date = 1_699_999_999; // seconds, not ms
        e.validate(NOW).unwrap();

        // A 3-digit value is too small to be seconds-since-epoch; left as-is and
        // rejected as pre-2000.
        let mut tiny = sample_sgv();
        tiny.date = 999;
        assert!(matches!(tiny.validate(NOW), Err(DocumentError::ImplausibleTimestamp(_))));

        // A genuine pre-2000 *millisecond* timestamp (1999-01-01) is a clock bug, not a
        // unit mismatch — outside the seconds band, so it's left unchanged and rejected.
        let mut pre_2000 = sample_sgv();
        pre_2000.date = 915_148_800_000; // 1999-01-01T00:00:00Z in ms
        assert!(matches!(pre_2000.validate(NOW), Err(DocumentError::ImplausibleTimestamp(_))));
    }

    /// A negative insulin dose is rejected — protects any downstream IOB maths.
    #[test]
    fn negative_insulin_is_rejected() {
        let t = Treatment {
            event_type: "Correction Bolus".into(),
            created_at: Some("2023-11-14T22:00:00Z".into()),
            date: None,
            glucose: None,
            glucose_type: None,
            units: None,
            carbs: None,
            protein: None,
            fat: None,
            insulin: Some(-2.0),
            duration: None,
            entered_by: None,
            notes: None,
            extra: Map::new(),
        };
        assert_eq!(
            t.validate(),
            Err(DocumentError::NotNonNegative { field: "insulin", value: -2.0 })
        );
    }

    /// A normal meal bolus treatment validates and preserves extra fields.
    #[test]
    fn valid_meal_bolus_passes() {
        let json = r#"{
            "eventType": "Meal Bolus",
            "created_at": "2023-11-14T18:30:00Z",
            "carbs": 45,
            "insulin": 4.5,
            "enteredBy": "Loop",
            "automatic": true
        }"#;
        let t: Treatment = serde_json::from_str(json).unwrap();
        t.validate().unwrap();
        assert_eq!(t.carbs, Some(45.0));
        assert_eq!(t.extra.get("automatic").unwrap(), true);
    }

    fn sample_sgv() -> Entry {
        Entry {
            entry_type: "sgv".into(),
            date: NOW - 1000,
            date_string: None,
            sgv: Some(112.0),
            direction: Some("Flat".into()),
            device: None,
            noise: None,
            filtered: None,
            unfiltered: None,
            rssi: None,
            units: None,
            extra: Map::new(),
        }
    }
}
