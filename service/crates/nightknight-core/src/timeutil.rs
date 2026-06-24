//! Dependency-free ISO-8601 ↔ epoch-milliseconds conversion.
//!
//! Nightscout records carry time as either a numeric epoch (`date`/`mills`) or an
//! ISO-8601 string (`created_at`, `dateString`). We need to convert between them
//! without pulling a date library into the wasm worker. Date arithmetic uses Howard
//! Hinnant's well-known `days_from_civil` / `civil_from_days` algorithms, which are
//! exact for all proleptic-Gregorian dates (correct leap-year handling included).
//!
//! Only UTC and fixed numeric offsets (`Z`, `+HH:MM`, `-HH:MM`) are handled — the
//! forms diabetes uploaders actually emit. Anything else returns `None` rather than
//! guessing, because a wrong timestamp puts a reading at the wrong point on the
//! chart.

/// Days from 1970-01-01 to the given proleptic-Gregorian date. Exact for all dates.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`]: a day count since 1970-01-01 back to `(y, m, d)`.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parse_int(s: &str) -> Option<i64> {
    s.parse::<i64>().ok()
}

/// Compose a UTC instant (epoch ms) from civil date/time components. Exact for all
/// proleptic-Gregorian dates. Shared by the ISO parser and the connector timestamp
/// parsers (Dexcom / LibreLinkUp).
pub fn ymd_hms_milli_to_ms(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    min: i64,
    sec: i64,
    milli: i64,
) -> i64 {
    let days = days_from_civil(year, month, day);
    (((days * 24 + hour) * 60 + min) * 60 + sec) * 1000 + milli
}

/// Parse an ISO-8601 timestamp into epoch milliseconds (UTC).
///
/// Accepts `YYYY-MM-DDTHH:MM:SS` with an optional `.fff` fraction and an optional
/// zone (`Z`, `+HH:MM`, `-HH:MM`); a space may replace the `T`. Returns `None` for
/// anything malformed or out of range.
pub fn parse_iso8601_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    // Split date and time on 'T' or ' '.
    let (date, rest) = s.split_once(['T', 't', ' '])?;
    let dparts: Vec<&str> = date.split('-').collect();
    if dparts.len() != 3 {
        return None;
    }
    let year = parse_int(dparts[0])?;
    let month = parse_int(dparts[1])?;
    let day = parse_int(dparts[2])?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Peel off the timezone suffix from the time portion.
    let (time, offset_ms) = if let Some(t) = rest.strip_suffix(['Z', 'z']) {
        (t, 0i64)
    } else if let Some(idx) = rest.rfind(['+', '-']) {
        // Guard against a '-' that is part of the time (there isn't one) — the offset
        // sign is always after the seconds, so rfind is safe here.
        let (t, off) = rest.split_at(idx);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let off = &off[1..];
        let (oh, om) = off.split_once(':')?;
        let off_ms = (parse_int(oh)? * 3600 + parse_int(om)? * 60) * 1000 * sign;
        (t, off_ms)
    } else {
        (rest, 0i64)
    };

    let tparts: Vec<&str> = time.split(':').collect();
    if tparts.len() < 2 {
        return None;
    }
    let hh = parse_int(tparts[0])?;
    let mm = parse_int(tparts[1])?;
    let (ss, millis) = if tparts.len() >= 3 {
        match tparts[2].split_once('.') {
            Some((s, frac)) => {
                // Normalise the fraction to exactly three digits (milliseconds).
                let mut frac = frac.to_string();
                frac.truncate(3);
                while frac.len() < 3 {
                    frac.push('0');
                }
                (parse_int(s)?, parse_int(&frac)?)
            }
            None => (parse_int(tparts[2])?, 0),
        }
    } else {
        (0, 0)
    };
    if !(0..=23).contains(&hh) || !(0..=59).contains(&mm) || !(0..=60).contains(&ss) {
        return None;
    }

    let ms = ymd_hms_milli_to_ms(year, month, day, hh, mm, ss, millis);
    Some(ms - offset_ms)
}

/// Format epoch milliseconds (UTC) as `YYYY-MM-DDTHH:MM:SS.fffZ`.
pub fn to_iso8601_ms(ms: i64) -> String {
    let (days, mut rem) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    let (y, m, d) = civil_from_days(days);
    let millis = rem % 1000;
    rem /= 1000;
    let ss = rem % 60;
    rem /= 60;
    let mm = rem % 60;
    let hh = rem / 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{millis:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Unix epoch itself must round-trip — the anchor of all the arithmetic.
    #[test]
    fn epoch_zero() {
        assert_eq!(parse_iso8601_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(to_iso8601_ms(0), "1970-01-01T00:00:00.000Z");
    }

    /// A concrete known instant (used throughout the tests) parses to its epoch ms.
    #[test]
    fn known_instant() {
        assert_eq!(parse_iso8601_ms("2023-11-14T22:13:19.000Z"), Some(1_699_999_999_000));
    }

    /// Numeric timezone offsets shift the instant correctly: 09:00+02:00 is the same
    /// instant as 07:00Z. A wrong offset would misplace a reading by hours.
    #[test]
    fn applies_timezone_offset() {
        let plus = parse_iso8601_ms("2023-06-01T09:00:00+02:00").unwrap();
        let utc = parse_iso8601_ms("2023-06-01T07:00:00Z").unwrap();
        assert_eq!(plus, utc);
        let minus = parse_iso8601_ms("2023-06-01T02:00:00-05:00").unwrap();
        assert_eq!(minus, parse_iso8601_ms("2023-06-01T07:00:00Z").unwrap());
    }

    /// Leap-day handling must be exact (2024 is a leap year, 2023 is not).
    #[test]
    fn leap_year_is_exact() {
        // 2024-02-29 exists; one day later is 2024-03-01.
        let feb29 = parse_iso8601_ms("2024-02-29T00:00:00Z").unwrap();
        let mar01 = parse_iso8601_ms("2024-03-01T00:00:00Z").unwrap();
        assert_eq!(mar01 - feb29, 86_400_000);
    }

    /// Round-tripping arbitrary instants through format→parse is lossless to the ms.
    #[test]
    fn round_trips_format_and_parse() {
        for ms in [0i64, 1_000, 1_699_999_999_123, 1_580_000_000_000, 4_102_444_800_000] {
            let s = to_iso8601_ms(ms);
            assert_eq!(parse_iso8601_ms(&s), Some(ms), "round-trip failed for {s}");
        }
    }

    /// Malformed input returns None rather than a plausible-looking wrong time.
    #[test]
    fn rejects_malformed() {
        assert_eq!(parse_iso8601_ms("not a date"), None);
        assert_eq!(parse_iso8601_ms("2023-13-01T00:00:00Z"), None); // month 13
        assert_eq!(parse_iso8601_ms("2023-01-01"), None); // no time
    }
}
