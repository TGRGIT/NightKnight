#if DEBUG
import Foundation

/// Synthetic, deterministic data for App Store screenshots / previews (and SwiftUI
/// previews). Compiled only in DEBUG, so it can never reach a release build.
///
/// Enabled by launching with the `-NKDemo` argument or `NK_DEMO=1` in the
/// environment (passed to the simulator via `SIMCTL_CHILD_NK_*`). It is consumed at
/// the `APIClient` layer, so the Dashboard, Analysis tab, Settings "Test connection"
/// and the Watch all light up identically with no network. Extra env vars steer
/// exactly what's on screen so each shot is one clean, reproducible launch:
///
///   NK_DEMO=1            turn demo mode on (or pass `-NKDemo`)
///   NK_UNIT=mmol|mgdl    preferred display unit
///   NK_PERIOD=1|7|14|30|90   dashboard trailing-summary period (days)
///   NK_TAB=0|1|2         start on Dashboard / Analysis / Settings
///   NK_ALARMS=1          show alarms as enabled (bell on)
///   NK_AUTOPLAY=1        sweep selectors + cycle tabs on a timer (for recording)
enum Demo {
    private static let env = ProcessInfo.processInfo.environment
    private static let args = ProcessInfo.processInfo.arguments

    static let isEnabled = args.contains("-NKDemo") || env["NK_DEMO"] == "1"
    static var alarms: Bool { env["NK_ALARMS"] == "1" }
    static var autoplay: Bool { env["NK_AUTOPLAY"] == "1" }
    static var initialTab: Int { Int(env["NK_TAB"] ?? "") ?? 0 }
    /// Section id to scroll the Analysis view to on launch (gri/core/agp/tod/episodes/advanced).
    static var scrollTarget: String? { env["NK_SCROLL"].flatMap { $0.isEmpty ? nil : $0 } }

    static var unit: GlucoseUnit {
        switch (env["NK_UNIT"] ?? "").lowercased() {
        case "mmol", "mmol/l": return .mmol
        default: return .mgdl
        }
    }
    static var period: TrailingPeriod {
        TrailingPeriod(rawValue: Int(env["NK_PERIOD"] ?? "") ?? 0) ?? .week
    }

    /// Apply demo preferences to the shared settings once, at launch.
    static func applyToSettings(_ s: Settings = .shared) {
        guard isEnabled else { return }
        s.preferredUnit = unit
        s.trailingDays = period.rawValue
        s.alarmsEnabled = alarms
        s.lowThresholdMgdl = 70
        s.highThresholdMgdl = 180
        // A friendly fake config so the app looks "connected", never a real host.
        s.baseURL = "https://nightknight.example.com"
        s.deviceToken = "demo"
    }

    // MARK: - Current reading (derived from the trace so the number, trend and
    // sparkline all agree)

    static func current(now: Date = Date()) -> CurrentReading {
        let r = readings(hours: 1, now: now)
        let last = r.last?.mgdl ?? 124
        // Slope over the last ~15 minutes → trend arrow.
        let prior = r.first(where: { $0.date >= now.addingTimeInterval(-15 * 60) })?.mgdl ?? last
        let d = last - prior
        let trend: TrendDirection
        switch abs(d) {
        case ..<5:    trend = .flat
        case ..<12:   trend = d > 0 ? .fortyFiveUp : .fortyFiveDown
        case ..<22:   trend = d > 0 ? .singleUp : .singleDown
        default:      trend = d > 0 ? .doubleUp : .doubleDown
        }
        return CurrentReading(date: now.addingTimeInterval(-75),
                              value: GlucoseValue(mgdl: last.rounded()),
                              trend: trend, trendLabel: trend.label)
    }

    // MARK: - Readings (a plausible, mostly-in-range day at 5-min CGM cadence)

    static func readings(hours: Int, now: Date = Date()) -> [GlucoseReading] {
        let step = 300.0                       // 5 minutes
        let count = max(2, hours * 12)
        var rng = SeededRNG(seed: 0x4E494748)  // "NIGH"
        let cal = Calendar.current
        var out: [GlucoseReading] = []
        out.reserveCapacity(count)
        for i in stride(from: count - 1, through: 0, by: -1) {
            let date = now.addingTimeInterval(-Double(i) * step)
            let h = Double(cal.component(.hour, from: date))
                + Double(cal.component(.minute, from: date)) / 60.0
            let jitter = (rng.nextUnit() - 0.5) * 9.0   // ±4.5 mg/dL sensor noise
            let mgdl = (dayCurve(hour: h) + jitter).rounded()
            out.append(GlucoseReading(date: date, value: GlucoseValue(mgdl: min(max(mgdl, 48), 288))))
        }
        return out
    }

    // MARK: - Analytics (full Statistical-Analysis set, varying by period)

    static func analytics(hours: Int, now: Date = Date()) -> GlucoseAnalytics {
        let days = max(1, Int((Double(hours) / 24).rounded()))
        let p = profile(forDays: days)
        let sd = p.mean * p.cv / 100
        // A1c estimates straight from the mean, matching the app's own formulas.
        let uGmi = 1 / (15.36 / p.mean + 0.0425)
        let gmi = 3.31 + 0.02392 * p.mean
        let eA1c = (p.mean + 46.7) / 28.7
        // GRI from the TIR split (Klonoff 2023), capped at 100.
        let hypo = 3.0 * (p.vLow + 0.8 * p.low)
        let hyper = 1.6 * (p.vHigh + 0.5 * p.high)
        let gri = min(100, hypo + hyper)
        let active = days >= 14 ? 98.0 : 97.0

        return GlucoseAnalytics(
            n: 288 * days * Int(active) / 100,
            meanMgdl: p.mean, sdMgdl: sd,
            uGmiPercent: uGmi, gmiPercent: gmi, estimatedA1cPercent: eA1c, cvPercent: p.cv,
            veryLowPct: p.vLow, lowPct: p.low, inRangePct: p.inRange, highPct: p.high, veryHighPct: p.vHigh,
            coverage: CoverageInfo(percentActive: active, daysCovered: Double(days),
                                   distinctDays: days, sufficient: days >= 14),
            gri: GriInfo(value: gri, zone: zone(gri), hypoComponent: hypo, hyperComponent: hyper),
            variability: VariabilityInfo(jIndex: 0.001 * pow(p.mean + sd, 2),
                                         mage: sd * 1.8, conga: sd * 1.05,
                                         modd: sd * 0.98, congaHours: 2),
            patterns: [
                PeriodInfo(startHour: 0, endHour: 6, n: 72 * days, meanMgdl: 108, inRangePct: 92),
                PeriodInfo(startHour: 6, endHour: 12, n: 72 * days, meanMgdl: p.mean + 8, inRangePct: 78),
                PeriodInfo(startHour: 12, endHour: 18, n: 72 * days, meanMgdl: p.mean - 4, inRangePct: 85),
                PeriodInfo(startHour: 18, endHour: 24, n: 72 * days, meanMgdl: p.mean + 16, inRangePct: 74),
            ],
            episodes: episodes(days: days, now: now))
    }

    // MARK: - AGP (percentile bands mirroring the day curve)

    static func agp(days: Int) -> [AgpBin] {
        stride(from: 0, through: 1410, by: 30).map { m in
            let h = Double(m) / 60.0
            let med = dayCurve(hour: h)
            let w = 16
                + peak(h, at: 7.5, height: 22, width: 1.8)
                + peak(h, at: 12.8, height: 18, width: 1.9)
                + peak(h, at: 19.0, height: 26, width: 2.1)
                - peak(h, at: 3.0, height: 6, width: 3.0)
            return AgpBin(minuteOfDay: m, n: max(1, days) * 9,
                          p05: max(54, med - 1.0 * w), p25: max(60, med - 0.5 * w),
                          p50: med, p75: med + 0.6 * w, p95: med + 1.5 * w)
        }
    }

    // MARK: - Profiles + helpers

    private struct Profile { let mean, cv, vLow, low, inRange, high, vHigh: Double }

    /// Good-but-realistic control that loosens a little over longer windows, so each
    /// period selection tells its own story.
    private static func profile(forDays days: Int) -> Profile {
        switch days {
        case ..<4:    return Profile(mean: 119, cv: 24, vLow: 0.0, low: 1.6, inRange: 90.4, high: 6.5, vHigh: 1.5)
        case ..<11:   return Profile(mean: 131, cv: 27, vLow: 0.4, low: 2.8, inRange: 84.3, high: 10.5, vHigh: 2.0)
        case ..<22:   return Profile(mean: 137, cv: 29, vLow: 0.6, low: 3.4, inRange: 80.0, high: 13.0, vHigh: 3.0)
        case ..<60:   return Profile(mean: 142, cv: 30, vLow: 0.8, low: 4.0, inRange: 77.5, high: 14.2, vHigh: 3.5)
        default:      return Profile(mean: 148, cv: 32, vLow: 1.0, low: 4.8, inRange: 74.0, high: 15.7, vHigh: 4.5)
        }
    }

    private static func episodes(days: Int, now: Date) -> EpisodesInfo {
        let d = Double(days)
        let low = EpisodeStat(count: Int(0.62 * d), nocturnal: max(1, Int(0.14 * d)),
                              perDay: 0.62, longestMin: 65, totalMin: 30 * d)
        let veryLow = EpisodeStat(count: max(0, Int(0.07 * d)), nocturnal: max(0, Int(0.05 * d)),
                                  perDay: 0.07, longestMin: 25, totalMin: 2 * d)
        let high = EpisodeStat(count: Int(1.1 * d), nocturnal: 0, perDay: 1.1, longestMin: 145, totalMin: 130 * d)
        let veryHigh = EpisodeStat(count: Int(0.2 * d), nocturnal: 0, perDay: 0.2, longestMin: 70, totalMin: 16 * d)
        // A short recent feed, newest first, over the last ~2 days.
        let recent: [RecentEpisode] = [
            RecentEpisode(kind: "high", start: now.addingTimeInterval(-3 * 3600), durationMin: 95, extremeMgdl: 214, nocturnal: false),
            RecentEpisode(kind: "low", start: now.addingTimeInterval(-9 * 3600), durationMin: 35, extremeMgdl: 63, nocturnal: false),
            RecentEpisode(kind: "high", start: now.addingTimeInterval(-22 * 3600), durationMin: 140, extremeMgdl: 241, nocturnal: false),
            RecentEpisode(kind: "low", start: now.addingTimeInterval(-30 * 3600), durationMin: 25, extremeMgdl: 58, nocturnal: true),
        ]
        return EpisodesInfo(low: low, veryLow: veryLow, high: high, veryHigh: veryHigh, recent: recent)
    }

    private static func zone(_ gri: Double) -> String {
        switch gri {
        case ..<20: return "A"
        case ..<40: return "B"
        case ..<60: return "C"
        case ..<80: return "D"
        default:    return "E"
        }
    }

    /// Smooth base glucose (mg/dL) by hour-of-day [0,24): calm overnight, dawn rise,
    /// three meals, and a gentle mid-afternoon dip toward the low threshold.
    private static func dayCurve(hour h: Double) -> Double {
        var v = 108.0
        v += 14 * sin((h - 6) / 24 * 2 * .pi)
        v += peak(h, at: 7.5, height: 78, width: 1.6)    // breakfast
        v += peak(h, at: 12.8, height: 60, width: 1.7)   // lunch
        v += peak(h, at: 19.0, height: 92, width: 2.0)   // dinner
        v -= peak(h, at: 16.0, height: 40, width: 1.3)   // afternoon dip
        v -= peak(h, at: 2.5, height: 12, width: 2.5)    // deepest overnight
        return v
    }

    private static func peak(_ x: Double, at c: Double, height: Double, width: Double) -> Double {
        height * exp(-pow(x - c, 2) / (2 * width * width))
    }

    /// Tiny deterministic PRNG (SplitMix64) so the trace is identical every run.
    private struct SeededRNG {
        var state: UInt64
        init(seed: UInt64) { state = seed }
        mutating func next() -> UInt64 {
            state &+= 0x9E3779B97F4A7C15
            var z = state
            z = (z ^ (z >> 30)) &* 0xBF58476D1CE4E5B9
            z = (z ^ (z >> 27)) &* 0x94D049BB133111EB
            return z ^ (z >> 31)
        }
        mutating func nextUnit() -> Double { Double(next() >> 11) / Double(1 << 53) }
    }
}
#endif
