//! The C ABI between `nightknight-core` and the iOS app — the **only** Rust↔Swift
//! binary contract (`ios/Rust/include/nightknight_ffi.h` mirrors these prototypes).
//!
//! Boundary rules, binding on every entry point:
//! * Every input is a NUL-terminated UTF-8 JSON C string.
//! * Every success returns a heap `char*` **owned by the caller**, who must free it
//!   with [`nk_free`] (one allocator, one free fn — never the C `free`).
//! * Failures come back **in-band** as `{"error":"…"}` JSON. A panic never crosses
//!   the boundary (undefined behaviour): each body runs under `catch_unwind` and a
//!   panic becomes `{"error":"panic"}`. The iOS build therefore uses the `ffi` cargo
//!   profile (`panic = "unwind"`), not the workspace release profile's `abort`.
//! * `NULL` is returned only when the result string itself cannot be allocated.
//! * Functions are pure: no global state, no threads, no IO.
//!
//! The analytics/AGP payloads delegate to [`nightknight_core::analytics::report`] —
//! the same composition the server's `/api/v4/analytics` and `/api/v4/agp` handlers
//! call — so the JSON the app decodes on-device is byte-identical to server output.

use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};

use serde::Deserialize;
use serde_json::{json, Value};

use nightknight_core::analytics::report;
use nightknight_core::analytics::{GlucoseReading, TirThresholds};
use nightknight_core::import::parse_glucose_csv;
use nightknight_core::units::{GlucoseUnit, GlucoseValue};

/// The FFI contract version. Bump on ANY change to the exported prototypes or the
/// JSON shapes they accept/emit; the app asserts it at launch against a Swift
/// constant so a stale checked-in xcframework fails loudly instead of surfacing as
/// silent DTO-decode blanks.
pub const ABI_VERSION: u32 = 1;

/// The FFI contract version (see [`ABI_VERSION`]).
#[no_mangle]
pub extern "C" fn nk_abi_version() -> u32 {
    ABI_VERSION
}

/// Free a string returned by any `nk_*` function. Passing `NULL` is a no-op.
///
/// # Safety
/// `ptr` must be a pointer previously returned by an `nk_*` function in this
/// library and not yet freed; anything else is undefined behaviour.
#[no_mangle]
pub unsafe extern "C" fn nk_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(CString::from_raw(ptr));
    }
}

/// The full Statistical-Analysis payload (the `/api/v4/analytics` body) computed
/// on-device. `readings_json` is `[{"date": <epoch ms>, "mgdl": <number>}, …]`
/// already restricted to the last `hours` hours; thresholds are mg/dL (pass
/// 54/70/180/250 for the consensus defaults).
///
/// # Safety
/// `readings_json` must be `NULL` or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn nk_analytics_json(
    readings_json: *const c_char,
    hours: i64,
    tz_offset_min: i64,
    very_low: f64,
    low: f64,
    high: f64,
    very_high: f64,
) -> *mut c_char {
    let input = input_string(readings_json);
    ffi_value(move || {
        let readings = readings_from_json(&input?)?;
        let t = TirThresholds { very_low, low, high, very_high };
        Ok(report::analytics_value(&readings, hours, tz_offset_min, &t))
    })
}

/// The Ambulatory Glucose Profile payload (the `/api/v4/agp` body) computed
/// on-device. `readings_json` as in [`nk_analytics_json`], already restricted to the
/// last `days` days.
///
/// # Safety
/// `readings_json` must be `NULL` or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn nk_agp_json(
    readings_json: *const c_char,
    days: i64,
    bin_minutes: i64,
    tz_offset_min: i64,
) -> *mut c_char {
    let input = input_string(readings_json);
    ffi_value(move || {
        let readings = readings_from_json(&input?)?;
        Ok(report::agp_value(&readings, days, bin_minutes, tz_offset_min))
    })
}

/// Parse a glucose CSV export (Dexcom Clarity or LibreView — auto-detected) for the
/// instant history backfill. Returns
/// `{"source":"dexcom"|"libreview","unit":…,"rows":…,"imported":…,"skipped":…,
///   "entries":[{"date":<epoch ms>,"mgdl":<number>},…]}` — entries are normalised to
/// mg/dL regardless of the export's unit, ready for the app's local store.
///
/// # Safety
/// `csv_text` must be `NULL` or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn nk_import_clarity_csv(
    csv_text: *const c_char,
    tz_offset_min: i64,
) -> *mut c_char {
    let input = input_string(csv_text);
    ffi_value(move || {
        let text = input?;
        let import = parse_glucose_csv(&text, tz_offset_min, None).map_err(|e| e.to_string())?;
        let entries: Vec<Value> = import
            .entries
            .iter()
            .filter_map(|e| {
                let date = e.get("date")?.as_i64()?;
                let sgv = e.get("sgv")?.as_f64()?;
                let unit = e
                    .get("units")
                    .and_then(|u| u.as_str())
                    .and_then(GlucoseUnit::parse)
                    .unwrap_or(GlucoseUnit::MgDl);
                let mgdl = GlucoseValue::new(sgv, unit).ok()?.mgdl();
                Some(json!({ "date": date, "mgdl": mgdl }))
            })
            .collect();
        Ok(json!({
            "source": import.source,
            "unit": import.unit,
            "rows": import.rows,
            "imported": import.imported,
            "skipped": import.skipped,
            "entries": entries,
        }))
    })
}

/// Copy a C-string input to an owned Rust string before any fallible work, so the
/// raw pointer never crosses into the `catch_unwind` body.
///
/// # Safety
/// `ptr` must be `NULL` or a valid NUL-terminated C string.
unsafe fn input_string(ptr: *const c_char) -> Result<String, String> {
    if ptr.is_null() {
        return Err("null input".into());
    }
    CStr::from_ptr(ptr)
        .to_str()
        .map(str::to_owned)
        .map_err(|_| "input is not valid UTF-8".into())
}

/// The shared safe shell: run the body under `catch_unwind`, fold errors and panics
/// into in-band `{"error":…}` JSON, and hand the caller an owned C string.
fn ffi_value(body: impl FnOnce() -> Result<Value, String>) -> *mut c_char {
    let value = match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => json!({ "error": e }),
        Err(_) => json!({ "error": "panic" }),
    };
    // serde_json escapes control characters, so the serialised text carries no
    // interior NUL; a CString failure here means allocation itself failed.
    let text = serde_json::to_string(&value).unwrap_or_else(|_| r#"{"error":"serialize"}"#.into());
    match CString::new(text) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Decode the `[{"date","mgdl"}]` reading rows the app's local store exports.
fn readings_from_json(s: &str) -> Result<Vec<GlucoseReading>, String> {
    #[derive(Deserialize)]
    struct Row {
        date: i64,
        mgdl: f64,
    }
    let rows: Vec<Row> =
        serde_json::from_str(s).map_err(|e| format!("bad readings JSON: {e}"))?;
    rows.into_iter()
        .map(|r| {
            GlucoseValue::from_mgdl(r.mgdl)
                .map(|v| GlucoseReading::new(r.date, v))
                .map_err(|e| format!("bad reading at {}: {e}", r.date))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Call an `nk_*` entry point through the real C-string round trip, copying the
    /// result out and freeing it with `nk_free` — the same contract Swift follows.
    fn round_trip(call: impl FnOnce(*const c_char) -> *mut c_char, input: &str) -> String {
        let c_in = CString::new(input).unwrap();
        let ptr = call(c_in.as_ptr());
        assert!(!ptr.is_null());
        let out = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
        unsafe { nk_free(ptr) };
        out
    }

    fn sample_readings_json(n: usize) -> String {
        let rows: Vec<Value> = (0..n)
            .map(|i| json!({ "date": (i as i64) * 300_000, "mgdl": 100.0 + (i % 40) as f64 }))
            .collect();
        serde_json::to_string(&rows).unwrap()
    }

    #[test]
    fn abi_version_is_stable() {
        assert_eq!(nk_abi_version(), ABI_VERSION);
    }

    /// The FFI output is byte-identical to calling the shared report module directly
    /// — the same guarantee the server's handlers rely on.
    #[test]
    fn analytics_round_trip_matches_report_module() {
        let input = sample_readings_json(288);
        let out = round_trip(|p| unsafe { nk_analytics_json(p, 24, 60, 54.0, 70.0, 180.0, 250.0) }, &input);
        let readings: Vec<GlucoseReading> = (0..288)
            .map(|i| {
                GlucoseReading::new(
                    (i as i64) * 300_000,
                    GlucoseValue::from_mgdl(100.0 + (i % 40) as f64).unwrap(),
                )
            })
            .collect();
        let direct = report::analytics_value(&readings, 24, 60, &TirThresholds::default());
        assert_eq!(out, serde_json::to_string(&direct).unwrap());
    }

    #[test]
    fn agp_round_trip_matches_report_module() {
        let input = sample_readings_json(288);
        let out = round_trip(|p| unsafe { nk_agp_json(p, 14, 15, 0) }, &input);
        let readings: Vec<GlucoseReading> = (0..288)
            .map(|i| {
                GlucoseReading::new(
                    (i as i64) * 300_000,
                    GlucoseValue::from_mgdl(100.0 + (i % 40) as f64).unwrap(),
                )
            })
            .collect();
        let direct = report::agp_value(&readings, 14, 15, 0);
        assert_eq!(out, serde_json::to_string(&direct).unwrap());
    }

    #[test]
    fn malformed_input_returns_in_band_error() {
        let out = round_trip(|p| unsafe { nk_analytics_json(p, 24, 0, 54.0, 70.0, 180.0, 250.0) }, "not json");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["error"].as_str().unwrap().contains("bad readings JSON"));

        let null_out = unsafe { nk_agp_json(std::ptr::null(), 14, 15, 0) };
        assert!(!null_out.is_null());
        let text = unsafe { CStr::from_ptr(null_out) }.to_str().unwrap().to_owned();
        unsafe { nk_free(null_out) };
        assert_eq!(text, r#"{"error":"null input"}"#);
    }

    /// A panic inside the body must come back as `{"error":"panic"}`, never unwind
    /// across the boundary. (The iOS build uses the `ffi` profile with
    /// `panic = "unwind"` so this shell works there exactly as under `cargo test`.)
    #[test]
    fn panic_is_folded_into_error_json() {
        let ptr = ffi_value(|| panic!("boom"));
        assert!(!ptr.is_null());
        let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
        unsafe { nk_free(ptr) };
        assert_eq!(text, r#"{"error":"panic"}"#);
    }

    #[test]
    fn clarity_csv_import_normalises_to_mgdl() {
        let csv = "Index,Timestamp (YYYY-MM-DDThh:mm:ss),Event Type,Event Subtype,Patient Info,Device Info,Source Device ID,Glucose Value (mg/dL),Insulin Value (u),Carb Value (grams),Duration (hh:mm:ss),Glucose Rate of Change (mg/dL/min),Transmitter Time (Long Integer)\n1,2024-01-01T00:00:00,EGV,,,,G6,120,,,,,\n2,2024-01-01T00:05:00,EGV,,,,G6,125,,,,,\n";
        let out = round_trip(|p| unsafe { nk_import_clarity_csv(p, 0) }, csv);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["source"], "dexcom");
        assert_eq!(v["imported"], 2);
        let entries = v["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["mgdl"], 120.0);
        assert!(entries[0]["date"].as_i64().unwrap() > 0);
    }

    #[test]
    fn unknown_csv_reports_in_band_error() {
        let out = round_trip(|p| unsafe { nk_import_clarity_csv(p, 0) }, "a,b,c\n1,2,3\n");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["error"].as_str().unwrap().contains("unrecognised"));
    }
}
