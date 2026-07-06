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
        // The add-widget gallery renders its representative preview via snapshot() with
        // isPreview == true; show the sample curve, not "--", even before the app is configured.
        if context.isPreview { return placeholder(in: context) }
        return await load()
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
        // A fresh snapshot read straight from the App Group, so we pick up credentials the app
        // changed since this (reused) extension process last ran — including a token the user
        // just cleared — without mutating the shared singleton from this background fetch.
        let settings = Settings.current()
        guard settings.isConfigured else {
            // Account removed: show "--", not the last cached reading from the old account.
            return GlucoseEntry(date: .now, value: nil, trend: .none, unit: settings.preferredUnit)
        }
        // Local-analytics sources (Dexcom/Libre/Nightscout): the APP is the sole fetcher —
        // a vendor login from every timeline reload would be slow, battery-hungry and (for
        // Libre especially) risks account lockout from re-authenticating across processes.
        // Render from the ReadingCache the app keeps warm; no sparkline (the cache holds
        // exactly one reading).
        if settings.usesLocalAnalytics {
            let cached = ReadingCache.load()
            return GlucoseEntry(date: .now, value: cached?.value, trend: cached?.trend ?? .none,
                                unit: settings.preferredUnit, readingDate: cached?.date)
        }
        let client = APIClient(settings: settings)
        async let curTask = client.current()
        async let entTask = client.entries(hours: 4)
        let fetched = (try? await curTask) ?? nil
        if let fetched { ReadingCache.save(fetched) }
        let readings = (try? await entTask) ?? []
        // Fall back to the last cached reading so a transient failure or CGM gap doesn't blank
        // the widget to "--" until the next (budget-throttled) refresh.
        let current = Self.reading(fetched: fetched, cached: ReadingCache.load(), isConfigured: true)
        return GlucoseEntry(date: .now, value: current?.value, trend: current?.trend ?? .none,
                            unit: settings.preferredUnit, readings: readings, readingDate: current?.date)
    }

    /// Decide which reading the widget should show. A fresh fetch wins; on a transient failure
    /// the last cached reading keeps the widget warm — but only while still configured, so a
    /// removed account drops to "--" instead of stale glucose. Pure, so it's unit-tested.
    static func reading(fetched: CurrentReading?, cached: CurrentReading?, isConfigured: Bool) -> CurrentReading? {
        guard isConfigured else { return nil }
        return fetched ?? cached
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

/// Band colours for the glance, per colour scheme. Dark keeps the vivid palette designed
/// for the `nkInk` tile (CarPlay / StandBy / dark home screens); light swaps to deeper
/// ink-on-paper variants so the hero value and sparkline hold contrast on the pale tile
/// instead of washing out (the vivid hexes drop under 2.5:1 on white).
enum GlanceColors {
    /// The `systemSmall` container tile: the dark brand ink at night, soft paper in light.
    static func tile(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? Color(red: 0.043, green: 0.055, blue: 0.071)   // nkInk #0B0E12
                        : Color(red: 0.969, green: 0.976, blue: 0.984)   // #F7F9FB
    }

    static func text(_ mgdl: Double, _ scheme: ColorScheme) -> Color {
        switch GlucoseBand.of(mgdl: mgdl) {
        case .veryLow, .veryHigh:
            return scheme == .dark ? Color(red: 1.0, green: 0.373, blue: 0.392)    // #FF5F64
                                   : Color(red: 0.851, green: 0.188, blue: 0.212)  // #D93036
        case .low, .high:
            return scheme == .dark ? Color(red: 0.949, green: 0.718, blue: 0.322)  // #F2B752
                                   : Color(red: 0.722, green: 0.459, blue: 0.078)  // #B87514
        case .inRange:
            return scheme == .dark ? Color(red: 0.275, green: 0.835, blue: 0.518)  // #46D584
                                   : Color(red: 0.090, green: 0.541, blue: 0.298)  // #178A4C
        }
    }

    static func line(_ mgdl: Double, _ scheme: ColorScheme) -> Color {
        switch GlucoseBand.of(mgdl: mgdl) {
        case .veryLow, .veryHigh:
            return scheme == .dark ? Color(red: 0.898, green: 0.282, blue: 0.302)  // #E5484D
                                   : Color(red: 0.851, green: 0.188, blue: 0.212)  // #D93036
        case .low, .high:
            return scheme == .dark ? Color(red: 0.878, green: 0.635, blue: 0.235)  // #E0A23C
                                   : Color(red: 0.722, green: 0.459, blue: 0.078)  // #B87514
        case .inRange:
            return scheme == .dark ? Color(red: 0.184, green: 0.745, blue: 0.416)  // #2FBE6A
                                   : Color(red: 0.090, green: 0.541, blue: 0.298)  // #178A4C
        }
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
    @Environment(\.colorScheme) private var scheme

    private var text: String { entry.value?.display(in: entry.unit) ?? "--" }
    private var color: Color { entry.value.map { GlanceColors.text($0.mgdl, scheme) } ?? .secondary }
    private var lineColor: Color { entry.value.map { GlanceColors.line($0.mgdl, scheme) } ?? .secondary }
    private var statusLabel: String? { entry.value.map { GlucoseBand.of(mgdl: $0.mgdl).label } }

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

        default: // .systemSmall — the CarPlay / home-screen glance (design "Layout A").
            ZStack(alignment: .topLeading) {
                // Filled band-coloured sparkline as a calm backdrop in the lower band, so
                // the big value always sits over a clean area (readability first).
                TrendSparkline(readings: entry.readings, lineColor: lineColor)
                    .padding(.top, 58)
                VStack(alignment: .leading, spacing: 0) {
                    // Hero: the value + trend arrow, band-coloured and bold.
                    HStack(alignment: .firstTextBaseline, spacing: 3) {
                        Text(text)
                            .font(.system(size: 50, weight: .heavy, design: .rounded))
                            .foregroundStyle(color)
                            .minimumScaleFactor(0.5).lineLimit(1)
                        Text(entry.trend.glyph)
                            .font(.system(size: 23, weight: .bold))
                            .foregroundStyle(color)
                    }
                    // Unit · trend wording (e.g. "mg/dL  Steady").
                    HStack(spacing: 6) {
                        Text(entry.unit.label).font(.caption2).foregroundStyle(.secondary)
                        if entry.trend != .none {
                            Text(entry.trend.label)
                                .font(.caption2).fontWeight(.bold).foregroundStyle(color)
                                .lineLimit(1).minimumScaleFactor(0.8)
                        }
                    }
                    Spacer(minLength: 0)
                    // Footer: status (dot + label) and reading freshness.
                    HStack(spacing: 5) {
                        if let statusLabel {
                            Circle().fill(color).frame(width: 7, height: 7)
                            Text(statusLabel)
                                .font(.caption2).fontWeight(.bold)
                                .lineLimit(1).minimumScaleFactor(0.7)
                        }
                        Spacer(minLength: 0)
                        if let d = entry.readingDate {
                            // Coarse "2 min ago" (no jittery seconds) — calmer for a glance and
                            // matches the design. Refreshes with the widget timeline (~5 min).
                            Text(d, format: .relative(presentation: .numeric, unitsStyle: .abbreviated))
                                .font(.system(size: 10)).foregroundStyle(.secondary)
                                .lineLimit(1).minimumScaleFactor(0.7)
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
    @Environment(\.colorScheme) private var scheme
    let entry: GlucoseEntry

    var body: some View {
        NightKnightWidgetContent(family: family, entry: entry)
            .containerBackground(for: .widget) { background }
    }

    /// The brand tile behind the home-screen / CarPlay `systemSmall` glance — dark ink
    /// (#0B0E12 = `nkInk`) in dark mode, soft paper in light — paired with the matching
    /// `GlanceColors` palette so the band colours read with the contrast the design
    /// intends in either appearance. Lock-screen accessory families are tinted monochrome
    /// by the system, so they stay transparent. (Avoids naming `.systemSmall`, which
    /// doesn't exist on watchOS.)
    @ViewBuilder private var background: some View {
        switch family {
        case .accessoryCircular, .accessoryInline, .accessoryRectangular:
            Color.clear
        default:
            GlanceColors.tile(scheme)
        }
    }
}

struct NightKnightWidget: Widget {
    var body: some WidgetConfiguration {
        AppIntentConfiguration(kind: "NightKnightWidget", intent: ConfigIntent.self, provider: Provider()) { entry in
            NightKnightWidgetView(entry: entry)
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
