import AppIntents
import SwiftUI
import WidgetKit

/// App-Intent-configured widget showing the latest glucose + trend over a subtle
/// background trendline. Supports home screen and Lock Screen / StandBy families. It
/// fetches via the shared `APIClient` (settings come from the App Group).
struct ConfigIntent: WidgetConfigurationIntent {
    static var title: LocalizedStringResource = "NightKnight Glucose"
    static var description = IntentDescription("Show your latest glucose reading.")
}

struct GlucoseEntry: TimelineEntry {
    let date: Date
    let value: GlucoseValue?
    let trend: TrendDirection
    let unit: GlucoseUnit
    /// Recent readings (last few hours) drawn as the background sparkline.
    var readings: [GlucoseReading] = []
    /// Time of the latest reading (for the freshness caption).
    var readingDate: Date? = nil
}

struct Provider: AppIntentTimelineProvider {
    func placeholder(in context: Context) -> GlucoseEntry {
        GlucoseEntry(date: .now, value: GlucoseValue(mgdl: 112), trend: .flat,
                     unit: Settings.shared.preferredUnit, readings: Self.sample, readingDate: .now)
    }

    func snapshot(for configuration: ConfigIntent, in context: Context) async -> GlucoseEntry {
        await load()
    }

    func timeline(for configuration: ConfigIntent, in context: Context) async -> Timeline<GlucoseEntry> {
        let entry = await load()
        // Refresh on the CGM cadence (~5 min) when we actually got a reading; if the fetch
        // came back empty (transient network, or creds not yet propagated to the App
        // Group), retry in ~2 min so the widget doesn't sit blank for a full cycle.
        let next: TimeInterval = entry.value == nil ? 120 : 300
        return Timeline(entries: [entry], policy: .after(.now.addingTimeInterval(next)))
    }

    private func load() async -> GlucoseEntry {
        let settings = Settings.shared
        let client = APIClient(settings: settings)
        async let curTask = client.current()
        async let entTask = client.entries(hours: 4)
        let fetched = (try? await curTask) ?? nil
        if let fetched { ReadingCache.save(fetched) }
        // Fall back to the last cached reading so a transient failure or CGM gap doesn't
        // blank the widget to "--" until the next (budget-throttled) refresh.
        let current = fetched ?? ReadingCache.load()
        let readings = (try? await entTask) ?? []
        return GlucoseEntry(date: .now, value: current?.value, trend: current?.trend ?? .none,
                            unit: settings.preferredUnit, readings: readings, readingDate: current?.date)
    }

    /// Build a timeline entry from a single reading (a fresh fetch, else the cache),
    /// without the recent-readings sparkline. Used to unit-test the cache fallback.
    static func entry(for reading: CurrentReading?, unit: GlucoseUnit) -> GlucoseEntry {
        GlucoseEntry(date: reading?.date ?? .now, value: reading?.value,
                     trend: reading?.trend ?? .none, unit: unit, readingDate: reading?.date)
    }

    /// A gentle synthetic curve for the gallery placeholder.
    static var sample: [GlucoseReading] {
        let base = Date.now.addingTimeInterval(-3 * 3600)
        let vals: [Double] = [96, 102, 110, 121, 134, 142, 138, 128, 117, 109, 104, 112]
        return vals.enumerated().map {
            GlucoseReading(date: base.addingTimeInterval(Double($0.offset) * 900),
                           value: GlucoseValue(mgdl: $0.element))
        }
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

/// A minimal, dependency-free sparkline of recent readings. Deliberately understated —
/// a thin line over a gradient that fades out toward the top, so a value placed in the
/// top-left stays crisp and the trendline reads as a calm backdrop.
struct TrendSparkline: View {
    let readings: [GlucoseReading]
    var lineColor: Color
    var filled: Bool = true

    var body: some View {
        GeometryReader { geo in
            let pts = Self.points(readings, size: geo.size)
            if pts.count >= 2 {
                ZStack {
                    if filled {
                        line(pts, closingTo: geo.size.height)
                            .fill(LinearGradient(
                                colors: [lineColor.opacity(0.32), lineColor.opacity(0.015)],
                                startPoint: .bottom, endPoint: .top))
                    }
                    line(pts, closingTo: nil)
                        .stroke(lineColor.opacity(filled ? 0.45 : 0.8),
                                style: StrokeStyle(lineWidth: 1.6, lineCap: .round, lineJoin: .round))
                    // A soft dot on the most recent point to anchor the eye.
                    let last = pts[pts.count - 1]
                    Circle().fill(lineColor.opacity(filled ? 0.7 : 1))
                        .frame(width: 4, height: 4)
                        .position(last)
                }
            }
        }
    }

    private func line(_ pts: [CGPoint], closingTo bottom: CGFloat?) -> Path {
        var p = Path()
        p.move(to: pts[0])
        for q in pts.dropFirst() { p.addLine(to: q) }
        if let bottom, let first = pts.first, let last = pts.last {
            p.addLine(to: CGPoint(x: last.x, y: bottom))
            p.addLine(to: CGPoint(x: first.x, y: bottom))
            p.closeSubpath()
        }
        return p
    }

    static func points(_ readings: [GlucoseReading], size: CGSize) -> [CGPoint] {
        let recent = readings.sorted { $0.date < $1.date }
        guard recent.count >= 2 else { return [] }
        let vals = recent.map(\.mgdl)
        let lo = (vals.min() ?? 80) - 6
        let hi = (vals.max() ?? 180) + 6
        let span = max(1, hi - lo)
        let t0 = recent.first!.date.timeIntervalSince1970
        let dt = max(1, recent.last!.date.timeIntervalSince1970 - t0)
        // Keep a hair of vertical inset so the line/dot never clips at the edges.
        let inset: CGFloat = 3
        let h = max(1, size.height - inset * 2)
        return recent.map { r in
            CGPoint(x: CGFloat((r.date.timeIntervalSince1970 - t0) / dt) * size.width,
                    y: inset + CGFloat(1 - (r.mgdl - lo) / span) * h)
        }
    }
}

/// Family-parameterised widget content. Split out from the `@Environment`-reading
/// wrapper below so it can be rendered for a specific `WidgetFamily` in tests — the
/// `widgetFamily` environment value is read-only and can't be injected directly.
struct NightKnightWidgetContent: View {
    let family: WidgetFamily
    let entry: GlucoseEntry

    private var text: String { entry.value?.display(in: entry.unit) ?? "--" }
    private var color: Color { entry.value.map { bandColor($0.mgdl) } ?? .secondary }

    var body: some View {
        switch family {
        case .accessoryInline:
            Text("\(text) \(entry.trend.glyph)")

        case .accessoryCircular:
            ZStack { Text(text).font(.system(.title3, design: .rounded)).bold() }

        case .accessoryRectangular:
            // Lock screen: value + a thin (untinted-fill) sparkline. Rendered monochrome
            // by the system, so the line tints with the lock-screen colour.
            HStack(spacing: 8) {
                VStack(alignment: .leading, spacing: 0) {
                    HStack(spacing: 3) {
                        Text(text).font(.system(.title2, design: .rounded)).bold()
                        Text(entry.trend.glyph).font(.body)
                    }
                    Text(entry.unit.label).font(.caption2).foregroundStyle(.secondary)
                }
                TrendSparkline(readings: entry.readings, lineColor: .primary, filled: false)
                    .frame(maxWidth: .infinity, maxHeight: 34)
            }

        default: // .systemSmall
            ZStack(alignment: .topLeading) {
                // Keep the trendline in the lower band so the big value always sits over
                // a clean area — readability first, the trend reads as a calm backdrop.
                TrendSparkline(readings: entry.readings, lineColor: color)
                    .padding(.top, 46)
                VStack(alignment: .leading, spacing: 1) {
                    HStack(alignment: .firstTextBaseline, spacing: 4) {
                        Text(text)
                            .font(.system(size: 44, weight: .bold, design: .rounded))
                            .foregroundStyle(color)
                            .minimumScaleFactor(0.7).lineLimit(1)
                        Text(entry.trend.glyph).font(.title3).foregroundStyle(color)
                    }
                    Text(entry.unit.label).font(.caption2).foregroundStyle(.secondary)
                    Spacer(minLength: 0)
                    HStack(spacing: 4) {
                        if entry.trend != .none {
                            Text(entry.trend.label).font(.caption2).fontWeight(.semibold).foregroundStyle(color)
                                .lineLimit(1).minimumScaleFactor(0.8)
                        }
                        Spacer(minLength: 0)
                        if let d = entry.readingDate {
                            Text(d, style: .relative).font(.caption2).foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
        }
    }
}

struct NightKnightWidgetView: View {
    @Environment(\.widgetFamily) private var family
    let entry: GlucoseEntry

    var body: some View { NightKnightWidgetContent(family: family, entry: entry) }
}

struct NightKnightWidget: Widget {
    var body: some WidgetConfiguration {
        AppIntentConfiguration(kind: "NightKnightWidget", intent: ConfigIntent.self, provider: Provider()) { entry in
            NightKnightWidgetView(entry: entry).containerBackground(.fill.tertiary, for: .widget)
        }
        .configurationDisplayName("NightKnight Glucose")
        .description("Your latest glucose reading and trend, over a recent trendline.")
        .supportedFamilies(Self.families)
    }

    // `.systemSmall` is iOS-only; this target may be compiled for watchOS too.
    static var families: [WidgetFamily] {
        #if os(watchOS)
        [.accessoryCircular, .accessoryInline, .accessoryRectangular]
        #else
        [.systemSmall, .accessoryCircular, .accessoryInline, .accessoryRectangular]
        #endif
    }
}
