//! LibreView CSV import.
//!
//! Parses a LibreView / FreeStyle Libre "Glucose" CSV export into canonical Nightscout
//! `sgv` entry bodies, ready for the normal ingest path (validation + content dedup).
//! Two facts about the format drive the parser:
//!
//! 1. **Timestamps are local device wall-clock with no zone.** The caller supplies a
//!    UTC offset (minutes east of UTC) so a reading at `08:15` becomes the right
//!    instant; we store epoch-ms UTC like every other reading.
//! 2. **Day/month order is locale-dependent** (`MM-DD-YYYY` in the US, `DD-MM-YYYY`
//!    elsewhere) and the file doesn't say which. We auto-detect from the data — any
//!    component > 12 disambiguates — and fall back to month-first (LibreView's US
//!    default), with an explicit override available.
//!
//! The parser is pure and total: malformed rows are *skipped and counted*, never
//! panic, so a partially-corrupt export still imports its good rows.

use serde_json::{json, Value};

use crate::timeutil;

/// Outcome of parsing a glucose CSV export (LibreView or Dexcom Clarity).
#[derive(Debug, Default, PartialEq)]
pub struct CsvImport {
    /// Canonical Nightscout `sgv` entry bodies, ready to ingest (deduped downstream).
    pub entries: Vec<Value>,
    /// Data rows seen (excludes preamble + header).
    pub rows: usize,
    /// Glucose rows turned into entries.
    pub imported: usize,
    /// Rows skipped (not a glucose record, empty, or unparseable).
    pub skipped: usize,
    /// The glucose unit detected from the header.
    pub unit: &'static str,
    /// The day/month order used (LibreView only; Dexcom is ISO/unambiguous).
    pub order: DateOrder,
    /// Which exporter the CSV was recognised as (`"libreview"` / `"dexcom"`).
    pub source: &'static str,
}

/// Day/month order of the ambiguous LibreView timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DateOrder {
    /// `MM-DD-YYYY` (LibreView's US default).
    #[default]
    MonthFirst,
    /// `DD-MM-YYYY`.
    DayFirst,
}

/// Why a glucose CSV could not be parsed at all (structural problems; bad *rows*
/// are skipped, not errors).
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    #[error("could not find the LibreView header row (no 'Device Timestamp' column)")]
    NoHeader,
    #[error("the header has no glucose column (Historic/Scan Glucose mg/dL or mmol/L)")]
    NoGlucoseColumn,
    #[error("unrecognised CSV — not a LibreView or Dexcom Clarity glucose export")]
    UnknownFormat,
}

/// Parse a glucose CSV, auto-detecting whether it is a LibreView or a Dexcom Clarity
/// export from its header, then dispatching to the matching parser. `utc_offset_min`
/// anchors the local timestamps; `order` overrides LibreView's day/month auto-detection.
pub fn parse_glucose_csv(
    text: &str,
    utc_offset_min: i64,
    order: Option<DateOrder>,
) -> Result<CsvImport, ImportError> {
    let records = parse_csv(text);
    let has = |needle: &str| {
        records.iter().take(40).any(|r| {
            r.iter().any(|f| f.trim().to_ascii_lowercase().contains(needle))
        })
    };
    // Dexcom Clarity exports carry an "Event Type" + "Glucose Value (mg/dL|mmol/L)"
    // header; LibreView carries "Device Timestamp". Check Dexcom first because some
    // LibreView locales also have a generic timestamp column.
    if has("event type") && has("glucose value") {
        parse_dexcom_csv(text, utc_offset_min)
    } else if has("device timestamp") {
        parse_libreview_csv(text, utc_offset_min, order)
    } else {
        Err(ImportError::UnknownFormat)
    }
}

/// Parse a LibreView CSV export. `utc_offset_min` anchors the local timestamps;
/// `order` overrides the day/month auto-detection when `Some`.
pub fn parse_libreview_csv(
    text: &str,
    utc_offset_min: i64,
    order: Option<DateOrder>,
) -> Result<CsvImport, ImportError> {
    let records = parse_csv(text);

    // The header is the first record carrying a "Device Timestamp" column; everything
    // before it is the export preamble (patient name, generated-on line).
    let header_idx = records
        .iter()
        .position(|r| r.iter().any(|f| f.trim().eq_ignore_ascii_case("Device Timestamp")))
        .ok_or(ImportError::NoHeader)?;
    let header = &records[header_idx];

    let exact = |name: &str| header.iter().position(|f| f.trim().eq_ignore_ascii_case(name));
    let ts_col = exact("Device Timestamp").ok_or(ImportError::NoHeader)?;
    let type_col = exact("Record Type");
    let device_col = exact("Device");
    let (historic_col, historic_unit) = glucose_col(header, "Historic Glucose");
    let (scan_col, scan_unit) = glucose_col(header, "Scan Glucose");
    if historic_col.is_none() && scan_col.is_none() {
        return Err(ImportError::NoGlucoseColumn);
    }
    let unit = historic_unit.or(scan_unit).unwrap_or("mg/dl");

    let data = &records[header_idx + 1..];
    let order = order.unwrap_or_else(|| detect_date_order(data, ts_col));

    let mut out = CsvImport { unit, order, source: "libreview", ..Default::default() };
    for r in data {
        // Skip blank lines and rows too short to hold a timestamp.
        if r.len() <= ts_col || r.iter().all(|f| f.trim().is_empty()) {
            continue;
        }
        out.rows += 1;

        // Record Type selects which glucose column holds the value: 0 = historic CGM,
        // 1 = manual scan (both → sgv). Other types (insulin/food/notes/strip/ketone)
        // carry no CGM glucose, so they're skipped. Older exports omit the column, in
        // which case the historic column is the reading.
        let rtype = type_col.and_then(|c| r.get(c)).and_then(|s| s.trim().parse::<i64>().ok());
        let value_col = match rtype {
            Some(0) => historic_col,
            Some(1) => scan_col,
            None => historic_col.or(scan_col),
            _ => None,
        };
        let Some(vc) = value_col else {
            out.skipped += 1;
            continue;
        };
        let Some(value) = r
            .get(vc)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
        else {
            out.skipped += 1;
            continue;
        };
        let Some(local_ms) = parse_timestamp(r[ts_col].trim(), order) else {
            out.skipped += 1;
            continue;
        };
        let date = local_ms - utc_offset_min * 60_000;
        let device = device_col
            .and_then(|c| r.get(c))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("LibreView");
        // mg/dL is integer-valued; mmol/L keeps one-decimal precision.
        let sgv = if unit == "mmol/l" { json!(value) } else { json!(value.round() as i64) };
        out.entries.push(json!({
            "type": "sgv",
            "date": date,
            "dateString": timeutil::to_iso8601_ms(date),
            "sgv": sgv,
            "units": unit,
            "device": format!("libreview/{device}"),
        }));
        out.imported += 1;
    }
    Ok(out)
}

/// Parse a Dexcom Clarity CSV export ("Glucose Values" / device download). Clarity rows
/// are typed by an `Event Type` column; only `EGV` rows (estimated glucose values) carry
/// a CGM reading. Timestamps are ISO-like local (`YYYY-MM-DDThh:mm:ss`), so the day/month
/// order is unambiguous. Out-of-range readings are exported as the words `Low`/`High`,
/// which Dexcom defines as below 40 / above 400 mg/dL — we clamp them to those limits.
pub fn parse_dexcom_csv(text: &str, utc_offset_min: i64) -> Result<CsvImport, ImportError> {
    let records = parse_csv(text);

    // The header is the first row that has both an "Event Type" and a "Glucose Value …"
    // column; anything before it is export preamble.
    let header_idx = records
        .iter()
        .position(|r| {
            let has = |needle: &str| r.iter().any(|f| f.trim().to_ascii_lowercase().contains(needle));
            has("event type") && has("glucose value")
        })
        .ok_or(ImportError::NoHeader)?;
    let header = &records[header_idx];

    let find = |pred: &dyn Fn(&str) -> bool| header.iter().position(|f| pred(&f.trim().to_ascii_lowercase()));
    let ts_col = find(&|f| f.starts_with("timestamp")).ok_or(ImportError::NoHeader)?;
    let type_col = find(&|f| f == "event type").ok_or(ImportError::NoHeader)?;
    let glu_col = find(&|f| f.starts_with("glucose value")).ok_or(ImportError::NoGlucoseColumn)?;
    let device_col = find(&|f| f.contains("source device id") || f == "device info");
    let unit = if header[glu_col].to_ascii_lowercase().contains("mmol") { "mmol/l" } else { "mg/dl" };

    let mut out = CsvImport { unit, order: DateOrder::MonthFirst, source: "dexcom", ..Default::default() };
    for r in &records[header_idx + 1..] {
        if r.len() <= ts_col.max(glu_col) || r.iter().all(|f| f.trim().is_empty()) {
            continue;
        }
        // Only estimated-glucose rows are CGM readings; calibrations, insulin, carbs,
        // exercise and notes are skipped (they aren't sgv).
        if !r.get(type_col).map(|s| s.trim().eq_ignore_ascii_case("EGV")).unwrap_or(false) {
            continue;
        }
        out.rows += 1;

        let raw = r.get(glu_col).map(|s| s.trim()).unwrap_or("");
        let value = match raw.to_ascii_lowercase().as_str() {
            "" => {
                out.skipped += 1;
                continue;
            }
            "low" => 40.0,
            "high" => 400.0,
            _ => match raw.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0) {
                Some(v) => v,
                None => {
                    out.skipped += 1;
                    continue;
                }
            },
        };
        let Some(local_ms) = parse_timestamp(r[ts_col].trim(), DateOrder::MonthFirst) else {
            out.skipped += 1;
            continue;
        };
        let date = local_ms - utc_offset_min * 60_000;
        let device = device_col
            .and_then(|c| r.get(c))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("Dexcom");
        let sgv = if unit == "mmol/l" { json!(value) } else { json!(value.round() as i64) };
        out.entries.push(json!({
            "type": "sgv",
            "date": date,
            "dateString": timeutil::to_iso8601_ms(date),
            "sgv": sgv,
            "units": unit,
            "device": format!("dexcom/{device}"),
        }));
        out.imported += 1;
    }
    Ok(out)
}

/// Find a glucose column by prefix (`"Historic Glucose"` / `"Scan Glucose"`) and read
/// its unit from the header suffix (`… mg/dL` / `… mmol/L`).
fn glucose_col(header: &[String], prefix: &str) -> (Option<usize>, Option<&'static str>) {
    let prefix = prefix.to_ascii_lowercase();
    for (i, f) in header.iter().enumerate() {
        let lf = f.trim().to_ascii_lowercase();
        if lf.starts_with(&prefix) {
            let unit = if lf.contains("mmol") {
                Some("mmol/l")
            } else if lf.contains("mg/dl") {
                Some("mg/dl")
            } else {
                None
            };
            return (Some(i), unit);
        }
    }
    (None, None)
}

/// Detect `MM-DD` vs `DD-MM` from the data: a first component > 12 means day-first; a
/// second component > 12 means month-first. Ambiguous data falls back to month-first.
fn detect_date_order(data: &[Vec<String>], ts_col: usize) -> DateOrder {
    for r in data {
        if let Some((a, b)) = r.get(ts_col).and_then(|ts| first_two_numbers(ts)) {
            // A 4-digit leading component is a year (ISO), which is unambiguous — skip.
            if a > 31 {
                continue;
            }
            if a > 12 {
                return DateOrder::DayFirst;
            }
            if b > 12 {
                return DateOrder::MonthFirst;
            }
        }
    }
    DateOrder::MonthFirst
}

/// Number of days in a (leap-year-aware) month; `0` for an invalid month.
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// The first two integer components of a timestamp string (separated by any non-digit).
fn first_two_numbers(s: &str) -> Option<(i64, i64)> {
    let mut nums = s
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.parse::<i64>().ok());
    Some((nums.next()?, nums.next()?))
}

/// Parse a LibreView timestamp (`MM-DD-YYYY HH:MM`, `DD-MM-YYYY HH:MM:SS`, optional
/// `AM/PM`, `-`/`/`/`.` date separators, optional leading `YYYY`) to local epoch-ms
/// (treated as UTC; the caller shifts by the real offset).
fn parse_timestamp(ts: &str, order: DateOrder) -> Option<i64> {
    let (date, rest) = ts.trim().split_once([' ', 'T'])?;
    let dp: Vec<&str> = date.split(['-', '/', '.']).filter(|p| !p.is_empty()).collect();
    if dp.len() != 3 {
        return None;
    }
    let (year, month, day) = if dp[0].len() == 4 {
        (dp[0].parse().ok()?, dp[1].parse().ok()?, dp[2].parse().ok()?)
    } else {
        let a: i64 = dp[0].parse().ok()?;
        let b: i64 = dp[1].parse().ok()?;
        let year: i64 = dp[2].parse().ok()?;
        let (month, day) = match order {
            DateOrder::MonthFirst => (a, b),
            DateOrder::DayFirst => (b, a),
        };
        (year, month, day)
    };

    let time = rest.trim();
    let (clock, ampm) = match time.rsplit_once(' ') {
        Some((c, ap)) if ap.eq_ignore_ascii_case("AM") || ap.eq_ignore_ascii_case("PM") => {
            (c.trim(), Some(ap.to_ascii_uppercase()))
        }
        _ => (time, None),
    };
    let tp: Vec<&str> = clock.split(':').collect();
    if tp.len() < 2 {
        return None;
    }
    let mut hour: i64 = tp[0].parse().ok()?;
    let min: i64 = tp[1].parse().ok()?;
    let sec: i64 = tp.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    if let Some(ap) = ampm {
        match ap.as_str() {
            "AM" if hour == 12 => hour = 0,
            "PM" if hour != 12 => hour += 12,
            _ => {}
        }
    }
    // Validate against the *actual* month length (leap-year aware) — otherwise an
    // impossible date like `02-31` would silently roll over into the next month instead
    // of being skipped, putting a reading at the wrong point on the chart.
    if !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&min)
        || !(0..=59).contains(&sec)
    {
        return None;
    }
    Some(timeutil::ymd_hms_milli_to_ms(year, month, day, hour, min, sec, 0))
}

/// A minimal RFC-4180 CSV reader: comma-separated fields, optional `"`-quoting with
/// `""` escapes, fields (incl. embedded commas/newlines) preserved. Returns records of
/// fields. Tolerant of `\r\n` and a missing trailing newline.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    let mut started = false; // whether the current record has any content yet
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                started = true;
                if in_quotes {
                    if chars.peek() == Some(&'"') {
                        field.push('"');
                        chars.next();
                    } else {
                        in_quotes = false;
                    }
                } else {
                    in_quotes = true;
                }
            }
            ',' if !in_quotes => {
                started = true;
                record.push(std::mem::take(&mut field));
            }
            '\r' if !in_quotes => {}
            '\n' if !in_quotes => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                started = false;
            }
            _ => {
                started = true;
                field.push(c);
            }
        }
    }
    if started || !field.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative US mg/dL export (month-first, 12-hour clock, a preamble line
    /// and the standard column header) imports its historic + scan readings and skips
    /// the insulin/notes rows.
    #[test]
    fn parses_us_mgdl_export() {
        let csv = "\
Fergus Cooney,Generated on,06-25-2026 9:00 AM
Device,Serial Number,Device Timestamp,Record Type,Historic Glucose mg/dL,Scan Glucose mg/dL,Rapid-Acting Insulin (units),Notes
FreeStyle LibreLink,abc123,06-24-2026 08:15 AM,0,95,,,
FreeStyle LibreLink,abc123,06-24-2026 08:30 AM,0,102,,,
FreeStyle LibreLink,abc123,06-24-2026 12:00 PM,1,,140,,
FreeStyle LibreLink,abc123,06-24-2026 12:05 PM,4,,,2.5,
FreeStyle LibreLink,abc123,06-24-2026 12:10 PM,6,,,,\"had lunch, felt low\"
";
        let r = parse_libreview_csv(csv, 0, None).unwrap();
        assert_eq!(r.unit, "mg/dl");
        assert_eq!(r.order, DateOrder::MonthFirst);
        assert_eq!(r.imported, 3, "2 historic + 1 scan");
        assert_eq!(r.skipped, 2, "insulin + note skipped");
        // First reading: 2026-06-24 08:15 local (offset 0) → that instant.
        let e0 = &r.entries[0];
        assert_eq!(e0["type"], "sgv");
        assert_eq!(e0["sgv"], 95);
        assert_eq!(e0["units"], "mg/dl");
        assert_eq!(e0["device"], "libreview/FreeStyle LibreLink");
        assert_eq!(e0["date"], timeutil::parse_iso8601_ms("2026-06-24T08:15:00Z").unwrap());
        // The quoted note with an embedded comma did not break field parsing.
        assert_eq!(r.entries.len(), 3);
    }

    /// A European DD-MM-YYYY 24-hour mmol/L export: the day-first order is auto-detected
    /// (day 25 > 12) and mmol values keep their unit + decimal.
    #[test]
    fn parses_eu_mmol_export_autodetects_day_first() {
        let csv = "\
Patient,Generated on,25-06-2026 21:00
Device,Serial Number,Device Timestamp,Record Type,Historic Glucose mmol/L,Scan Glucose mmol/L
FreeStyle Libre 3,zzz,25-06-2026 21:15,0,5.5,
FreeStyle Libre 3,zzz,25-06-2026 21:30,0,6.1,
";
        let r = parse_libreview_csv(csv, 60, None).unwrap(); // CET = +60 min
        assert_eq!(r.unit, "mmol/l");
        assert_eq!(r.order, DateOrder::DayFirst);
        assert_eq!(r.imported, 2);
        assert_eq!(r.entries[0]["sgv"], 5.5);
        assert_eq!(r.entries[0]["units"], "mmol/l");
        // 21:15 CET (+60) → 20:15 UTC.
        assert_eq!(
            r.entries[0]["date"],
            timeutil::parse_iso8601_ms("2026-06-25T20:15:00Z").unwrap()
        );
    }

    /// Timestamp parsing covers both locales, both clocks, and the noon/midnight edges.
    #[test]
    fn timestamp_parsing_edges() {
        let mdy = DateOrder::MonthFirst;
        assert_eq!(
            parse_timestamp("06-24-2026 12:00 AM", mdy),
            timeutil::parse_iso8601_ms("2026-06-24T00:00:00Z")
        );
        assert_eq!(
            parse_timestamp("06-24-2026 12:00 PM", mdy),
            timeutil::parse_iso8601_ms("2026-06-24T12:00:00Z")
        );
        assert_eq!(
            parse_timestamp("06-24-2026 23:30", mdy),
            timeutil::parse_iso8601_ms("2026-06-24T23:30:00Z")
        );
        // Day-first with slashes and seconds.
        assert_eq!(
            parse_timestamp("24/06/2026 08:15:30", DateOrder::DayFirst),
            timeutil::parse_iso8601_ms("2026-06-24T08:15:30Z")
        );
        // ISO leading year is detected regardless of order.
        assert_eq!(
            parse_timestamp("2026-06-24 08:15", mdy),
            timeutil::parse_iso8601_ms("2026-06-24T08:15:00Z")
        );
        assert_eq!(parse_timestamp("not a date", mdy), None);
        assert_eq!(parse_timestamp("13-13-2026 08:00", mdy), None); // month 13
        // Impossible calendar days are rejected (not silently rolled over).
        assert_eq!(parse_timestamp("02-31-2026 08:00", mdy), None); // Feb 31
        assert_eq!(parse_timestamp("04-31-2026 08:00", mdy), None); // Apr 31
        assert_eq!(parse_timestamp("02-29-2025 08:00", mdy), None); // 2025 not a leap year
        assert!(parse_timestamp("02-29-2024 08:00", mdy).is_some()); // 2024 leap year — valid
    }

    /// Malformed and empty input never panics; bad rows are skipped and counted.
    #[test]
    fn malformed_input_is_safe() {
        // No header at all.
        assert_eq!(parse_libreview_csv("just,some,garbage\n1,2,3", 0, None), Err(ImportError::NoHeader));
        // Header but no glucose column.
        let no_glu = "Device,Device Timestamp,Record Type\nx,06-24-2026 08:00,0";
        assert_eq!(parse_libreview_csv(no_glu, 0, None), Err(ImportError::NoGlucoseColumn));
        // A valid header with junk rows imports nothing but doesn't panic.
        let junky = "\
Device,Serial Number,Device Timestamp,Record Type,Historic Glucose mg/dL
x,s,not-a-date,0,95
x,s,06-24-2026 08:00,0,not-a-number
x,s,06-24-2026 08:05,0,
";
        let r = parse_libreview_csv(junky, 0, None).unwrap();
        assert_eq!(r.imported, 0);
        assert_eq!(r.rows, 3);
        assert_eq!(r.skipped, 3);
    }

    /// An explicit order override beats auto-detection (for genuinely ambiguous dates,
    /// e.g. all components ≤ 12).
    #[test]
    fn explicit_order_override() {
        let csv = "\
Device,Serial Number,Device Timestamp,Record Type,Historic Glucose mg/dL
Libre,s,03-04-2026 08:00,0,100
";
        let mdy = parse_libreview_csv(csv, 0, Some(DateOrder::MonthFirst)).unwrap();
        assert_eq!(mdy.entries[0]["date"], timeutil::parse_iso8601_ms("2026-03-04T08:00:00Z").unwrap());
        let dmy = parse_libreview_csv(csv, 0, Some(DateOrder::DayFirst)).unwrap();
        assert_eq!(dmy.entries[0]["date"], timeutil::parse_iso8601_ms("2026-04-03T08:00:00Z").unwrap());
    }

    /// A representative Dexcom Clarity export: the patient/device preamble rows and the
    /// non-EGV event rows are skipped; only the EGV readings become entries. `Low`/`High`
    /// out-of-range words clamp to Dexcom's 40 / 400 mg/dL reporting limits.
    #[test]
    fn parses_dexcom_clarity_export() {
        let csv = "\
Index,Timestamp (YYYY-MM-DDThh:mm:ss),Event Type,Event Subtype,Patient Info,Device Info,Source Device ID,Glucose Value (mg/dL),Insulin Value (u),Carb Value (grams),Duration (hh:mm:ss),Transmitter Time (Long Integer),Transmitter ID
1,,FirstName,,Jane,,,,,,,,
2,,LastName,,Doe,,,,,,,,
3,2024-06-01T00:03:00,EGV,,,Dexcom G6,Dexcom G6,112,,,,,8GAAAA
4,2024-06-01T00:08:00,EGV,,,Dexcom G6,Dexcom G6,108,,,,,8GAAAA
5,2024-06-01T03:00:00,EGV,,,Dexcom G6,Dexcom G6,Low,,,,,8GAAAA
6,2024-06-01T09:00:00,EGV,,,Dexcom G6,Dexcom G6,High,,,,,8GAAAA
7,2024-06-01T10:00:00,Insulin,Fast-Acting,,Dexcom G6,Dexcom G6,,5,,,,
8,2024-06-01T11:00:00,Calibration,,,Dexcom G6,Dexcom G6,120,,,,,8GAAAA
";
        let r = parse_dexcom_csv(csv, 0).unwrap();
        assert_eq!(r.source, "dexcom");
        assert_eq!(r.unit, "mg/dl");
        assert_eq!(r.imported, 4, "4 EGV rows");
        assert_eq!(r.rows, 4, "calibration/insulin are not EGV → not counted as rows");
        let e0 = &r.entries[0];
        assert_eq!(e0["sgv"], 112);
        assert_eq!(e0["device"], "dexcom/Dexcom G6");
        assert_eq!(e0["date"], timeutil::parse_iso8601_ms("2024-06-01T00:03:00Z").unwrap());
        assert_eq!(r.entries[2]["sgv"], 40, "Low → 40");
        assert_eq!(r.entries[3]["sgv"], 400, "High → 400");
    }

    /// Dexcom mmol/L exports keep the unit and decimal value.
    #[test]
    fn parses_dexcom_mmol() {
        let csv = "\
Index,Timestamp (YYYY-MM-DDThh:mm:ss),Event Type,Event Subtype,Patient Info,Device Info,Source Device ID,Glucose Value (mmol/L)
1,2024-06-01T00:03:00,EGV,,,Dexcom G7,Dexcom G7,6.2
";
        let r = parse_dexcom_csv(csv, 120).unwrap(); // +120 min offset
        assert_eq!(r.unit, "mmol/l");
        assert_eq!(r.entries[0]["sgv"], 6.2);
        // 00:03 local at +120 → previous day 22:03 UTC.
        assert_eq!(r.entries[0]["date"], timeutil::parse_iso8601_ms("2024-05-31T22:03:00Z").unwrap());
    }

    /// The auto-detecting dispatcher routes each format to the right parser and rejects
    /// anything that is neither.
    #[test]
    fn dispatcher_detects_format() {
        let libre = "\
Device,Serial Number,Device Timestamp,Record Type,Historic Glucose mg/dL
Libre,s,06-24-2026 08:00,0,100
";
        let dexcom = "\
Index,Timestamp (YYYY-MM-DDThh:mm:ss),Event Type,Glucose Value (mg/dL)
1,2024-06-01T00:03:00,EGV,112
";
        assert_eq!(parse_glucose_csv(libre, 0, None).unwrap().source, "libreview");
        assert_eq!(parse_glucose_csv(dexcom, 0, None).unwrap().source, "dexcom");
        assert_eq!(parse_glucose_csv("a,b,c\n1,2,3", 0, None), Err(ImportError::UnknownFormat));
    }
}
