import Foundation

/// Last-known glucose reading, persisted in the shared App Group so a widget or watch
/// complication can keep showing a value when a refresh fails — a transient network
/// error, a Cloudflare Access blip, or a normal CGM gap — instead of blanking to "--".
///
/// Written by the app on every successful poll *and* by the widget on every successful
/// fetch, so whichever runs more recently keeps the cache warm. The reading carries its
/// real timestamp (`CurrentReading.date`), so callers can show/age it if they want.
enum ReadingCache {
    private static var defaults: UserDefaults { UserDefaults(suiteName: Settings.appGroup) ?? .standard }

    static func save(_ reading: CurrentReading) {
        let d = defaults
        d.set(reading.value.mgdl, forKey: Key.mgdl)
        d.set(reading.trend.rawValue, forKey: Key.trend)
        d.set(reading.date.timeIntervalSince1970, forKey: Key.date)
    }

    static func load() -> CurrentReading? {
        let d = defaults
        guard let epoch = d.object(forKey: Key.date) as? Double else { return nil }
        let trend = TrendDirection(rawValue: d.string(forKey: Key.trend) ?? "") ?? .none
        return CurrentReading(
            date: Date(timeIntervalSince1970: epoch),
            value: GlucoseValue(mgdl: d.double(forKey: Key.mgdl)),
            trend: trend,
            // The cache doesn't persist the server's label; fall back to the local one.
            trendLabel: trend.label)
    }

    private enum Key {
        static let mgdl = "cache.mgdl", trend = "cache.trend", date = "cache.date"
    }
}
