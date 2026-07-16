//! Exportable reports — the machine-readable side of NightKnight's analytics.
//!
//! Two self-describing artefacts, both scoped to a caller-chosen date range and stamped
//! with the generation time so a shared file explains itself:
//!
//! * [`readings_csv`] — the raw sensor readings as CSV (one row per reading, oldest
//!   first), for a spreadsheet or re-import.
//! * [`metrics_json`] — the full computed metric set (the `/analytics` payload plus the
//!   AGP percentile bands) wrapped with range + generation metadata, for a clinician's
//!   tooling or a printable AGP one-pager.
//!
//! Like the sibling [`report`](super::report) module this is the **single source of
//! truth** shared verbatim by the server's `GET /api/v4/export` and the iOS clients, so
//! an export produced on-device carries byte-identical numbers to the server's. The
//! metric maths lives in the parent [`analytics`](crate::analytics) module; this owns
//! only the file composition (which columns, which wrapper fields, how it is labelled).

use serde_json::{json, Value};

use super::report;
use super::{GlucoseReading, TirThresholds};
use crate::timeutil::{self, DAY_MS};
use crate::units::GlucoseUnit;

/// The default AGP bin width (minutes) used by the JSON metrics export — 15 minutes → 96
/// bins across the day, the Bergenstal AGP standard, matching `/api/v4/agp`'s default.
pub const DEFAULT_AGP_BIN_MINUTES: i64 = 15;

/// One hour / one day in milliseconds — window arithmetic for the range helpers.
const HOUR_MS: i64 = 3_600_000;

/// The date range and generation metadata every export is labelled with. All instants are
/// epoch **milliseconds**; `tz` is minutes east of UTC — the caller's local clock, used
/// both for the AGP/time-of-day maths and for the local timestamps written into the CSV.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportRange {
    /// Inclusive window start (epoch ms).
    pub start_ms: i64,
    /// Inclusive window end (epoch ms).
    pub end_ms: i64,
    /// When the export was generated (epoch ms).
    pub generated_ms: i64,
    /// Minutes east of UTC.
    pub tz: i64,
}

impl ExportRange {
    /// Whole days the window spans (at least 1), rounded up — the AGP day count.
    pub fn days(&self) -> i64 {
        let span = (self.end_ms - self.start_ms).max(0);
        ((span + DAY_MS - 1) / DAY_MS).max(1)
    }

    /// Whole hours the window spans (at least 1), rounded up — the analytics window and
    /// coverage denominator.
    pub fn hours(&self) -> i64 {
        let span = (self.end_ms - self.start_ms).max(0);
        ((span + HOUR_MS - 1) / HOUR_MS).max(1)
    }
}

/// A safe base filename stem (no extension) for an export over this range, e.g.
/// `nightknight-readings-2024-01-01_2024-01-14`. Dates are the local calendar days of the
/// window bounds, so the name alone tells a clinician what period the file covers. Callers
/// append `.csv` / `.json`.
pub fn filename_stem(kind: &str, range: &ExportRange) -> String {
    let start = timeutil::date_string_from_day_number(timeutil::day_number(range.start_ms, range.tz));
    let end = timeutil::date_string_from_day_number(timeutil::day_number(range.end_ms, range.tz));
    format!("nightknight-{kind}-{start}_{end}")
}

/// Serialise the raw sensor readings in the window as CSV, oldest first.
///
/// The file opens with a `#`-prefixed metadata preamble (which pandas' `read_csv(comment='#')`,
/// R's `read.csv(comment.char='#')` and similar skip) that names the export, its generation
/// time, its local date range and reading count — satisfying "clearly labelled with date
/// range and generation timestamp" *inside* the file itself, not just the download name.
///
/// Columns: `timestamp` (local ISO-8601 with numeric offset — the wall-clock time the
/// person saw), `epoch_ms` (the canonical UTC instant), `mg_dL` (integer) and `mmol_L`
/// (one decimal). Both units are emitted so the file is unit-agnostic. Every field is a
/// number or a timestamp NightKnight generated — there is no user-controlled free text — so
/// there is no CSV-injection surface.
pub fn readings_csv(readings: &[GlucoseReading], range: &ExportRange) -> String {
    let mut rows: Vec<&GlucoseReading> = readings.iter().collect();
    rows.sort_by_key(|r| r.date_ms);

    let mut out = String::with_capacity(rows.len() * 48 + 512);
    out.push_str("# NightKnight glucose export \u{2014} raw sensor readings\n");
    out.push_str(&format!("# generated: {}\n", timeutil::to_iso8601_ms(range.generated_ms)));
    out.push_str(&format!(
        "# range: {} .. {}\n",
        timeutil::to_iso8601_offset(range.start_ms, range.tz),
        timeutil::to_iso8601_offset(range.end_ms, range.tz),
    ));
    out.push_str(&format!("# readings: {}\n", rows.len()));
    out.push_str("# NOT A MEDICAL DEVICE \u{2014} for personal/clinical review, not treatment decisions.\n");
    out.push_str("timestamp,epoch_ms,mg_dL,mmol_L\n");
    for r in rows {
        out.push_str(&timeutil::to_iso8601_offset(r.date_ms, range.tz));
        out.push(',');
        out.push_str(&r.date_ms.to_string());
        out.push(',');
        out.push_str(&r.value.mgdl_rounded().to_string());
        out.push(',');
        out.push_str(&format!("{:.1}", r.value.display(GlucoseUnit::Mmol)));
        out.push('\n');
    }
    out
}

/// Assemble the full computed metric set for the window as a self-describing JSON object.
///
/// Bundles the entire `/api/v4/analytics` payload ([`report::analytics_value`] — GRI,
/// Time-in-Range count + time-weighted, GMI/uGMI/eA1c, SD/CV, J-index, MAGE, CONGA, MODD,
/// time-of-day patterns and the hypo/hyper episode roll-ups + recent event list) alongside
/// the AGP percentile bands ([`report::agp_value`]), all wrapped with the generation time,
/// the local date range and the TIR thresholds the numbers were computed against. Because
/// it delegates to the shared `report` module the exported figures are byte-identical to
/// what the live Statistical-Analysis view shows.
///
/// `readings` must already be restricted to the window; `bin_minutes` is the AGP bin width.
pub fn metrics_json(
    readings: &[GlucoseReading],
    range: &ExportRange,
    bin_minutes: i64,
    t: &TirThresholds,
) -> Value {
    let hours = range.hours();
    let days = range.days();
    json!({
        "report": "NightKnight glucose metrics export",
        "notMedicalDevice": true,
        "generated": {
            "ms": range.generated_ms,
            "iso": timeutil::to_iso8601_ms(range.generated_ms),
        },
        "range": {
            "startMs": range.start_ms,
            "endMs": range.end_ms,
            "start": timeutil::to_iso8601_offset(range.start_ms, range.tz),
            "end": timeutil::to_iso8601_offset(range.end_ms, range.tz),
            "tzOffset": range.tz,
            "days": days,
            "hours": hours,
        },
        "thresholds": {
            "veryLow": t.very_low,
            "low": t.low,
            "high": t.high,
            "veryHigh": t.very_high,
        },
        "analytics": report::analytics_value(readings, hours, range.tz, t),
        "agp": report::agp_value(readings, days, bin_minutes, range.tz),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::GlucoseValue;

    fn at(ms: i64, mgdl: f64) -> GlucoseReading {
        GlucoseReading::new(ms, GlucoseValue::from_mgdl(mgdl).unwrap())
    }

    /// A fixed range: 14 local days ending at a stable instant, UTC.
    fn range() -> ExportRange {
        let end = 19_500 * DAY_MS; // midnight UTC on a stable day
        ExportRange { start_ms: end - 14 * DAY_MS, end_ms: end, generated_ms: end, tz: 0 }
    }

    /// The window helpers round *up* to whole hours/days so a partial window never
    /// under-reports its span (which sets the coverage denominator and AGP day count).
    #[test]
    fn range_days_and_hours_round_up() {
        let r = ExportRange { start_ms: 0, end_ms: DAY_MS + 1, generated_ms: 0, tz: 0 };
        assert_eq!(r.days(), 2);
        let r2 = ExportRange { start_ms: 0, end_ms: HOUR_MS * 3 + 1, generated_ms: 0, tz: 0 };
        assert_eq!(r2.hours(), 4);
        // A zero-width window still reports at least one day / hour, never zero.
        let z = ExportRange { start_ms: 5, end_ms: 5, generated_ms: 0, tz: 0 };
        assert_eq!(z.days(), 1);
        assert_eq!(z.hours(), 1);
    }

    /// The filename stem carries the local date range so the download name alone says what
    /// period the file covers.
    #[test]
    fn filename_stem_carries_the_local_date_range() {
        let end = 19_500 * DAY_MS;
        let r = ExportRange { start_ms: end - 2 * DAY_MS, end_ms: end, generated_ms: end, tz: 0 };
        let stem = filename_stem("readings", &r);
        let end_date = timeutil::date_string_from_day_number(19_500);
        let start_date = timeutil::date_string_from_day_number(19_498);
        assert_eq!(stem, format!("nightknight-readings-{start_date}_{end_date}"));
    }

    /// The CSV opens with a labelled `#` preamble, then a header row, then one row per
    /// reading, oldest first, with both units.
    #[test]
    fn readings_csv_is_labelled_ordered_and_dual_unit() {
        let r = range();
        // Intentionally out of order to prove the export sorts oldest-first.
        let readings = vec![at(r.start_ms + 600_000, 180.0), at(r.start_ms, 90.0)];
        let csv = readings_csv(&readings, &r);
        let lines: Vec<&str> = csv.lines().collect();
        assert!(lines[0].starts_with("# NightKnight glucose export"));
        assert!(csv.contains("# generated:"));
        assert!(csv.contains("# range:"));
        assert!(csv.contains("# readings: 2"));
        assert!(csv.contains("NOT A MEDICAL DEVICE"));
        // The header is the first non-comment line.
        let header = lines.iter().find(|l| !l.starts_with('#')).unwrap();
        assert_eq!(*header, "timestamp,epoch_ms,mg_dL,mmol_L");
        // Data rows follow the header, oldest first: 90 mg/dL row precedes the 180 row.
        let data: Vec<&&str> = lines.iter().filter(|l| !l.starts_with('#') && **l != *header).collect();
        assert_eq!(data.len(), 2);
        assert!(data[0].contains(",90,"), "first data row is the oldest reading (90 mg/dL)");
        assert!(data[0].ends_with(",5.0"), "90 mg/dL is 5.0 mmol/L");
        assert!(data[1].contains(",180,"));
        assert!(data[1].ends_with(",10.0"), "180 mg/dL is 10.0 mmol/L");
        // Each data row carries the epoch ms too.
        assert!(data[0].contains(&r.start_ms.to_string()));
    }

    /// An empty window still yields a valid CSV: the preamble + header, zero data rows.
    #[test]
    fn readings_csv_handles_empty_window() {
        let csv = readings_csv(&[], &range());
        assert!(csv.contains("# readings: 0"));
        let data_rows = csv
            .lines()
            .filter(|l| !l.starts_with('#') && *l != "timestamp,epoch_ms,mg_dL,mmol_L")
            .count();
        assert_eq!(data_rows, 0);
    }

    /// The JSON export is labelled with the generation time + local range and embeds the
    /// full analytics payload and the AGP bands, byte-identical to the shared `report`
    /// module (so the export can never drift from the live view).
    #[test]
    fn metrics_json_labels_and_embeds_the_full_metric_set() {
        let r = range();
        let readings: Vec<GlucoseReading> =
            (0..288).map(|i| at(r.start_ms + i * 300_000, 120.0 + (i % 40) as f64)).collect();
        let t = TirThresholds::default();
        let v = metrics_json(&readings, &r, DEFAULT_AGP_BIN_MINUTES, &t);

        assert_eq!(v["report"], "NightKnight glucose metrics export");
        assert_eq!(v["notMedicalDevice"], true);
        assert_eq!(v["generated"]["ms"], r.generated_ms);
        assert_eq!(v["generated"]["iso"], timeutil::to_iso8601_ms(r.generated_ms));
        assert_eq!(v["range"]["startMs"], r.start_ms);
        assert_eq!(v["range"]["endMs"], r.end_ms);
        assert_eq!(v["range"]["days"], 14);
        assert_eq!(v["thresholds"]["low"], 70.0);

        // The embedded analytics/AGP are exactly what the shared report module produces.
        assert_eq!(v["analytics"], report::analytics_value(&readings, r.hours(), r.tz, &t));
        assert_eq!(v["agp"], report::agp_value(&readings, r.days(), DEFAULT_AGP_BIN_MINUTES, r.tz));
    }
}
