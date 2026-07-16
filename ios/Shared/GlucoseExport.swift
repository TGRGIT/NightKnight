import Foundation

/// The date range + generation metadata every export is labelled with — the Swift mirror
/// of the server's `analytics::export::ExportRange`. All instants are epoch milliseconds;
/// `tz` is minutes east of UTC (the device's local clock, used for the local timestamps in
/// the CSV and for labelling). Producing exports on-device this way lets a standalone
/// source (Dexcom Share / LibreLinkUp / Nightscout) share the same reports as server mode.
struct ExportRange {
    let startMs: Int64
    let endMs: Int64
    let generatedMs: Int64
    let tz: Int

    /// A trailing window of `days` ending now, in the device's timezone — how the Analysis
    /// view's period selector maps onto an export range.
    static func trailing(days: Int, now: Date = Date()) -> ExportRange {
        let end = Int64((now.timeIntervalSince1970 * 1000).rounded())
        return ExportRange(
            startMs: end - Int64(days) * 86_400_000,
            endMs: end,
            generatedMs: end,
            tz: APIClient.tzOffsetMinutes
        )
    }

    /// Whole days the window spans (at least 1), matching how the selected period reads.
    var days: Int { max(1, Int((Double(endMs - startMs) / 86_400_000).rounded())) }
}

/// Machine-readable exports of a user's glucose data, matching the server's
/// `nightknight-core::analytics::export` format so a file produced on-device reads the
/// same as one from `GET /api/v4/export`.
enum GlucoseExport {
    static let disclaimer = "NOT A MEDICAL DEVICE \u{2014} for personal/clinical review, not treatment decisions."

    // MARK: CSV (raw readings)

    /// The raw sensor readings in the window as CSV, oldest first, with a labelled `#`
    /// preamble (naming the export, its generation time, its range and reading count — the
    /// same lines pandas/R skip) then `timestamp,epoch_ms,mg_dL,mmol_L`. Both units are
    /// emitted so the file is unit-agnostic; every field is a value we generate, so there is
    /// no CSV-injection surface.
    static func readingsCSV(_ readings: [GlucoseReading], range: ExportRange) -> String {
        let rows = readings.sorted { $0.date < $1.date }
        var out = ""
        out += "# NightKnight glucose export \u{2014} raw sensor readings\n"
        out += "# generated: \(isoUTC(range.generatedMs))\n"
        out += "# range: \(isoOffset(range.startMs, range.tz)) .. \(isoOffset(range.endMs, range.tz))\n"
        out += "# readings: \(rows.count)\n"
        out += "# \(disclaimer)\n"
        out += "timestamp,epoch_ms,mg_dL,mmol_L\n"
        for r in rows {
            let ms = Int64((r.date.timeIntervalSince1970 * 1000).rounded())
            let mgdl = Int(r.value.mgdl.rounded())
            let mmol = String(format: "%.1f", r.value.mmol)
            out += "\(isoOffset(ms, range.tz)),\(ms),\(mgdl),\(mmol)\n"
        }
        return out
    }

    // MARK: JSON (computed metric set)

    /// The full computed metric set for the window as a self-describing JSON document:
    /// the analytics (GRI, Time-in-Range, GMI/uGMI/eA1c, SD/CV, J-index/MAGE/CONGA/MODD,
    /// time-of-day patterns and episode roll-ups) plus the AGP percentile bands, wrapped
    /// with the generation time and local date range. Field names mirror the server's
    /// `/analytics` payload so an iOS export and a server export line up key-for-key.
    static func metricsJSON(analytics a: GlucoseAnalytics, agp: [AgpBin], range: ExportRange) -> Data {
        // Every optional metric encodes as its number or JSON null (never omitted), so the
        // export shape is stable. Explicit `[String: Any]` annotations keep Swift from
        // rejecting the heterogeneous literals.
        func opt(_ v: Double?) -> Any { v.map { $0 as Any } ?? NSNull() }
        func optI(_ v: Int?) -> Any { v.map { $0 as Any } ?? NSNull() }
        func optS(_ v: String?) -> Any { v.map { $0 as Any } ?? NSNull() }
        func stat(_ s: EpisodeStat) -> [String: Any] {
            ["count": s.count, "nocturnal": s.nocturnal, "perDay": s.perDay,
             "longestMin": s.longestMin, "totalMin": s.totalMin]
        }

        let tir: [String: Any] = [
            "veryLowPct": a.veryLowPct, "lowPct": a.lowPct, "inRangePct": a.inRangePct,
            "highPct": a.highPct, "veryHighPct": a.veryHighPct,
        ]
        let coverage: [String: Any] = [
            "percentActive": opt(a.coverage.percentActive),
            "daysCovered": opt(a.coverage.daysCovered),
            "distinctDays": optI(a.coverage.distinctDays),
            "sufficient": a.coverage.sufficient,
        ]
        let gri: [String: Any] = [
            "value": opt(a.gri.value), "zone": optS(a.gri.zone),
            "hypoComponent": opt(a.gri.hypoComponent), "hyperComponent": opt(a.gri.hyperComponent),
        ]
        let variability: [String: Any] = [
            "jIndex": opt(a.variability.jIndex), "mage": opt(a.variability.mage),
            "conga": opt(a.variability.conga), "modd": opt(a.variability.modd),
            "congaHours": opt(a.variability.congaHours),
        ]
        let patterns: [[String: Any]] = a.patterns.map { p in
            ["startHour": p.startHour, "endHour": p.endHour, "n": p.n,
             "meanMgdl": opt(p.meanMgdl), "inRangePct": opt(p.inRangePct)]
        }
        let episodes: [String: Any] = [
            "low": stat(a.episodes.low), "veryLow": stat(a.episodes.veryLow),
            "high": stat(a.episodes.high), "veryHigh": stat(a.episodes.veryHigh),
        ]
        let analytics: [String: Any] = [
            "n": a.n,
            "meanMgdl": opt(a.meanMgdl), "sdMgdl": opt(a.sdMgdl),
            "uGmiPercent": opt(a.uGmiPercent), "gmiPercent": opt(a.gmiPercent),
            "estimatedA1cPercent": opt(a.estimatedA1cPercent), "cvPercent": opt(a.cvPercent),
            "timeInRange": tir, "coverage": coverage, "gri": gri,
            "variability": variability, "patterns": patterns, "episodes": episodes,
        ]
        let bins: [[String: Any]] = agp.map { b in
            ["minuteOfDay": b.minuteOfDay, "n": b.n,
             "p05": opt(b.p05), "p25": opt(b.p25), "p50": opt(b.p50), "p75": opt(b.p75), "p95": opt(b.p95)]
        }
        let generated: [String: Any] = ["ms": range.generatedMs, "iso": isoUTC(range.generatedMs)]
        let rangeObj: [String: Any] = [
            "startMs": range.startMs, "endMs": range.endMs,
            "start": isoOffset(range.startMs, range.tz), "end": isoOffset(range.endMs, range.tz),
            "tzOffset": range.tz, "days": range.days,
        ]
        let agpObj: [String: Any] = ["binMinutes": 15, "bins": bins]
        let obj: [String: Any] = [
            "report": "NightKnight glucose metrics export",
            "notMedicalDevice": true,
            "generated": generated,
            "range": rangeObj,
            "analytics": analytics,
            "agp": agpObj,
        ]
        return (try? JSONSerialization.data(withJSONObject: obj, options: [.prettyPrinted, .sortedKeys]))
            ?? Data("{}".utf8)
    }

    // MARK: timestamp helpers

    /// Local ISO-8601 with numeric offset, e.g. `2024-01-01T09:30:00+02:00` (or `…Z` at UTC).
    private static func isoOffset(_ ms: Int64, _ tzMin: Int) -> String {
        let f = ISO8601DateFormatter()
        f.timeZone = TimeZone(secondsFromGMT: tzMin * 60) ?? TimeZone(identifier: "UTC")!
        f.formatOptions = [.withInternetDateTime]
        return f.string(from: Date(timeIntervalSince1970: Double(ms) / 1000))
    }

    /// UTC ISO-8601, e.g. `2024-01-01T07:30:00Z`.
    private static func isoUTC(_ ms: Int64) -> String {
        let f = ISO8601DateFormatter()
        f.timeZone = TimeZone(identifier: "UTC")!
        f.formatOptions = [.withInternetDateTime]
        return f.string(from: Date(timeIntervalSince1970: Double(ms) / 1000))
    }
}
