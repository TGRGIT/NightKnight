import AppIntents
import SwiftUI
import WidgetKit

/// Apple Watch complications (WidgetKit accessory families) showing the latest
/// glucose + trend. App-Intent configured, fetched via the shared `APIClient`.
struct WatchConfigIntent: WidgetConfigurationIntent {
    static var title: LocalizedStringResource = "NightKnight Glucose"
    static var description = IntentDescription("Show your latest glucose reading.")
}

struct WatchEntry: TimelineEntry {
    let date: Date
    let value: GlucoseValue?
    let trend: TrendDirection
    let unit: GlucoseUnit
}

struct WatchProvider: AppIntentTimelineProvider {
    func placeholder(in context: Context) -> WatchEntry {
        WatchEntry(date: .now, value: GlucoseValue(mgdl: 110), trend: .flat, unit: Settings.shared.preferredUnit)
    }
    func snapshot(for configuration: WatchConfigIntent, in context: Context) async -> WatchEntry {
        // Show the sample in the gallery preview rather than "--" before the watch is configured.
        if context.isPreview { return placeholder(in: context) }
        return await load()
    }
    func timeline(for configuration: WatchConfigIntent, in context: Context) async -> Timeline<WatchEntry> {
        Timeline(entries: [await load()], policy: .after(.now.addingTimeInterval(300)))
    }
    private func load() async -> WatchEntry {
        // A fresh snapshot read straight from the App Group: picks up a token the user just
        // edited/cleared on the phone (synced here via WatchConnectivity) without mutating the
        // shared singleton from this background fetch.
        let settings = Settings.current()
        guard settings.dataSource != nil else {
            // No source chosen (or removed): show "--", not a stale cached reading.
            return WatchEntry(date: .now, value: nil, trend: .none, unit: settings.preferredUnit)
        }
        // Local-analytics sources: the watch never holds vendor credentials (they are
        // deliberately not synced from the phone) and must not log in per timeline
        // reload (lockout risk) — render the reading the phone pushed into this
        // watch's ReadingCache via WatchConnectivity. `isConfigured`'s credential
        // check must NOT gate this branch: for these sources it always reads the
        // watch's own (always-empty) vendor fields and would blank the complication
        // even with a perfectly healthy phone-side connection.
        if settings.usesLocalAnalytics {
            let cached = ReadingCache.load()
            return WatchEntry(date: cached?.date ?? .now, value: cached?.value,
                              trend: cached?.trend ?? .none, unit: settings.preferredUnit)
        }
        guard settings.isConfigured else {
            return WatchEntry(date: .now, value: nil, trend: .none, unit: settings.preferredUnit)
        }
        let fetched = try? await APIClient(settings: settings).current()
        if let fetched { ReadingCache.save(fetched) }
        // Fall back to the last cached reading so a transient failure or CGM gap doesn't
        // blank the complication to "--".
        let reading = fetched ?? ReadingCache.load()
        return WatchEntry(date: reading?.date ?? .now, value: reading?.value,
                          trend: reading?.trend ?? .none, unit: settings.preferredUnit)
    }

    // Required on watchOS; no gallery recommendations.
    func recommendations() -> [AppIntentRecommendation<WatchConfigIntent>] { [] }
}

private func bandColor(_ mgdl: Double) -> Color {
    switch GlucoseBand.of(mgdl: mgdl) {
    case .veryLow, .veryHigh: return .red
    case .low, .high: return .orange
    case .inRange: return .green
    }
}

struct WatchComplicationView: View {
    @Environment(\.widgetFamily) private var family
    let entry: WatchEntry

    private var text: String { entry.value?.display(in: entry.unit) ?? "--" }
    private var color: Color { entry.value.map { bandColor($0.mgdl) } ?? .secondary }

    var body: some View {
        switch family {
        case .accessoryInline:
            Text("\(text) \(entry.trend.glyph)")
        case .accessoryCorner:
            Text(text).foregroundStyle(color)
        case .accessoryRectangular:
            HStack {
                VStack(alignment: .leading, spacing: 1) {
                    Text("NightKnight").font(.caption2).foregroundStyle(.secondary)
                    Text("\(text) \(entry.trend.glyph)").font(.headline).foregroundStyle(color)
                }
                Spacer()
            }
        default: // accessoryCircular
            VStack(spacing: 0) {
                Text(text).font(.system(.title3, design: .rounded)).bold().foregroundStyle(color)
                Text(entry.trend.glyph).font(.caption2).foregroundStyle(.secondary)
            }
        }
    }
}

struct NightKnightWatchComplication: Widget {
    var body: some WidgetConfiguration {
        AppIntentConfiguration(kind: "NightKnightWatchComplication", intent: WatchConfigIntent.self, provider: WatchProvider()) { entry in
            WatchComplicationView(entry: entry).containerBackground(.clear, for: .widget)
        }
        .configurationDisplayName("NightKnight")
        .description("Latest glucose and trend.")
        .supportedFamilies([.accessoryCircular, .accessoryInline, .accessoryRectangular, .accessoryCorner])
    }
}

@main
struct NightKnightWatchWidgetBundle: WidgetBundle {
    var body: some Widget { NightKnightWatchComplication() }
}
