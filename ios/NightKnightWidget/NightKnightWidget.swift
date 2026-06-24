import AppIntents
import SwiftUI
import WidgetKit

/// App-Intent-configured widget showing the latest glucose + trend. Supports home
/// screen and Lock Screen / StandBy families. It fetches via the shared `APIClient`
/// (settings come from the App Group).
struct ConfigIntent: WidgetConfigurationIntent {
    static var title: LocalizedStringResource = "NightKnight Glucose"
    static var description = IntentDescription("Show your latest glucose reading.")
}

struct GlucoseEntry: TimelineEntry {
    let date: Date
    let value: GlucoseValue?
    let trend: TrendDirection
    let unit: GlucoseUnit
}

struct Provider: AppIntentTimelineProvider {
    func placeholder(in context: Context) -> GlucoseEntry {
        GlucoseEntry(date: .now, value: GlucoseValue(mgdl: 110), trend: .flat, unit: Settings.shared.preferredUnit)
    }

    func snapshot(for configuration: ConfigIntent, in context: Context) async -> GlucoseEntry {
        await load()
    }

    func timeline(for configuration: ConfigIntent, in context: Context) async -> Timeline<GlucoseEntry> {
        let entry = await load()
        // CGM cadence: refresh in ~5 minutes.
        return Timeline(entries: [entry], policy: .after(.now.addingTimeInterval(300)))
    }

    private func load() async -> GlucoseEntry {
        let settings = Settings.shared
        let current = try? await APIClient(settings: settings).current()
        return GlucoseEntry(date: .now, value: current?.value, trend: current?.trend ?? .none, unit: settings.preferredUnit)
    }

    // Required on watchOS (has a default only on iOS); no gallery recommendations.
    func recommendations() -> [AppIntentRecommendation<ConfigIntent>] { [] }
}

private func bandColor(_ mgdl: Double) -> Color {
    switch GlucoseBand.of(mgdl: mgdl) {
    case .veryLow, .veryHigh: return .red
    case .low, .high: return .orange
    case .inRange: return .green
    }
}

struct NightKnightWidgetView: View {
    @Environment(\.widgetFamily) private var family
    let entry: GlucoseEntry

    private var text: String { entry.value?.display(in: entry.unit) ?? "--" }
    private var color: Color { entry.value.map { bandColor($0.mgdl) } ?? .secondary }

    var body: some View {
        switch family {
        case .accessoryInline:
            Text("\(text) \(entry.trend.glyph)")
        case .accessoryCircular:
            ZStack { Text(text).font(.system(.title3, design: .rounded)).bold() }
        default:
            VStack(alignment: .leading, spacing: 2) {
                Text("NightKnight").font(.caption2).foregroundStyle(.secondary)
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(text).font(.system(size: 40, weight: .bold, design: .rounded)).foregroundStyle(color)
                    Text(entry.trend.glyph).font(.title2)
                }
                Text(entry.unit.label).font(.caption2).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        }
    }
}

struct NightKnightWidget: Widget {
    var body: some WidgetConfiguration {
        AppIntentConfiguration(kind: "NightKnightWidget", intent: ConfigIntent.self, provider: Provider()) { entry in
            NightKnightWidgetView(entry: entry).containerBackground(.fill.tertiary, for: .widget)
        }
        .configurationDisplayName("NightKnight Glucose")
        .description("Your latest glucose reading and trend.")
        .supportedFamilies(Self.families)
    }

    // `.systemSmall` is iOS-only; this target may be compiled for watchOS too.
    private static var families: [WidgetFamily] {
        #if os(watchOS)
        [.accessoryCircular, .accessoryInline, .accessoryRectangular]
        #else
        [.systemSmall, .accessoryCircular, .accessoryInline, .accessoryRectangular]
        #endif
    }
}

@main
struct NightKnightWidgetBundle: WidgetBundle {
    var body: some Widget { NightKnightWidget() }
}
