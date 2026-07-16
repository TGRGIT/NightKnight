import SwiftUI
import WidgetKit

@MainActor
@Observable
final class DashboardModel {
    var current: CurrentReading?
    var readings: [GlucoseReading] = []
    var analytics: GlucoseAnalytics?
    var errorText: String?
    /// Trailing-summary period; remembered across launches via `Settings`.
    var period: TrailingPeriod { didSet { settings.trailingDays = period.rawValue } }
    let chartHours = 24

    let settings = Settings.shared
    private var client: APIClient { APIClient(settings: settings) }

    /// Epoch-seconds of the reading we last reloaded widgets for, so foreground polling
    /// doesn't request a reload every 60s when nothing changed.
    private var lastWidgetReloadAt: TimeInterval = 0

    init() {
        period = TrailingPeriod(rawValue: Settings.shared.trailingDays) ?? .week
    }

    func refresh() async {
        do {
            async let c = client.current()
            async let e = client.entries(hours: chartHours)
            async let a = client.analytics(hours: period.hours)
            current = try await c
            readings = try await e
            analytics = try await a
            errorText = nil
            if let current { AlarmManager.shared.evaluate(current, settings: settings) }
            if settings.writeToHealthKit { await HealthKitManager.shared.write(readings) }
            // Keep the widgets in step, but only when the reading actually advanced —
            // reloading every 60s of foreground polling (when the widget isn't even
            // visible) would burn through WidgetKit's reload budget for nothing.
            if let current, current.date.timeIntervalSince1970 > lastWidgetReloadAt {
                lastWidgetReloadAt = current.date.timeIntervalSince1970
                WidgetCenter.shared.reloadAllTimelines()
                // Same freshness gate for the watch push: in a local-analytics source
                // this is the watch's only data feed.
                PhoneSyncManager.shared.pushReading(current)
            }
        } catch {
            errorText = (error as? APIError)?.errorDescription ?? error.localizedDescription
        }
    }

    func reloadAnalytics() async {
        analytics = try? await client.analytics(hours: period.hours)
    }
}

struct DashboardView: View {
    /// Owned by `RootTabView` so the launch splash can watch for the first live reading.
    @Bindable var model: DashboardModel
    private var unit: GlucoseUnit { model.settings.preferredUnit }
    private let metricCols = [GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10)]

    var body: some View {
        NavigationStack {
            // One screen, no scrolling: current reading + trailing summary are sized to
            // their content and the chart absorbs the remaining height.
            VStack(spacing: 10) {
                currentCard
                summaryCard
                chartCard.frame(maxHeight: .infinity)
            }
            .padding(12)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Color.nkInk.ignoresSafeArea())
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    NightKnightLogo(height: 40)
                        .accessibilityLabel("NightKnight")
                }
                ToolbarItem(placement: .topBarLeading) { alarmButton }
            }
            .task { await pollLoop() }
            #if DEBUG
            .task { await autoplaySweep() }
            #endif
        }
        .tint(Color.nkAccent)
    }

    #if DEBUG
    /// Preview recording: sweep the trailing-period selector so the summary animates.
    private func autoplaySweep() async {
        guard Demo.autoplay else { return }
        try? await Task.sleep(for: .seconds(1.5))
        let seq: [TrailingPeriod] = [.day, .week, .twoWeeks, .month, .quarter]
        while !Task.isCancelled {
            for p in seq {
                withAnimation { model.period = p }
                try? await Task.sleep(for: .seconds(1.5))
            }
        }
    }
    #endif

    /// Quick alarm on/off. Tapping off silences all alarms immediately; turning on
    /// requests notification permission. (No snooze — alarms are simply on or off.)
    private var alarmButton: some View {
        Button {
            let turnOn = !model.settings.alarmsEnabled
            model.settings.alarmsEnabled = turnOn
            if turnOn { Task { await AlarmManager.shared.requestAuth() } }
        } label: {
            Image(systemName: model.settings.alarmsEnabled ? "bell.fill" : "bell.slash")
        }
        .tint(model.settings.alarmsEnabled ? Color.nkAccent : Color.nkMuted)
        .accessibilityIdentifier("alarmToggle")
        .accessibilityLabel(model.settings.alarmsEnabled ? "Alarms on" : "Alarms off")
    }

    /// Refresh now, then once a minute (CGM cadence) while the view is alive.
    private func pollLoop() async {
        while !Task.isCancelled {
            await model.refresh()
            try? await Task.sleep(for: .seconds(60))
        }
    }

    // MARK: - Cards

    private var currentCard: some View {
        let band = model.current.map { GlucoseBand.of(mgdl: $0.value.mgdl) }
        // The spotlight sparkline shows ONLY the last hour of readings, if there are any.
        let lastHour = model.readings.filter { $0.date.timeIntervalSinceNow > -3600 }
        return HStack(alignment: .center, spacing: 14) {
            Text(model.current?.value.display(in: unit) ?? "--")
                .font(.system(size: 58, weight: .bold, design: .rounded))
                .foregroundStyle(band?.color ?? .primary)
                .lineLimit(1).minimumScaleFactor(0.6)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(unit.label).font(.subheadline).foregroundStyle(.secondary)
                    Text(model.current?.trend.glyph ?? "·").font(.title3)
                        .foregroundStyle(band?.color ?? .secondary)
                }
                if let c = model.current {
                    if c.trend != .none {
                        Text(c.trendLabel).font(.caption).fontWeight(.semibold)
                            .foregroundStyle(band?.color ?? .secondary)
                    }
                    Text(c.date, style: .relative).font(.caption2).foregroundStyle(.secondary)
                } else if let err = model.errorText {
                    Text(err).font(.caption2).foregroundStyle(Color.nkAccent).lineLimit(2)
                }
            }
            Spacer(minLength: 8)
            if lastHour.count >= 2 {
                MiniSparkline(readings: lastHour, color: band?.color ?? .secondary)
                    .frame(width: 112, height: 46)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.nkTile, in: RoundedRectangle(cornerRadius: 18))
    }

    private var summaryCard: some View {
        let a = model.analytics
        return VStack(spacing: 10) {
            Picker("Period", selection: $model.period) {
                ForEach(TrailingPeriod.allCases) { Text($0.label).tag($0) }
            }
            .pickerStyle(.segmented)
            .onChange(of: model.period) { Task { await model.reloadAnalytics() } }

            LazyVGrid(columns: metricCols, spacing: 10) {
                // Lead with uGMI (the preferred A1c estimate); the sub names GMI + eA1c so
                // it's unambiguous which figure is which — never just a bare "A1c".
                metric("uGMI", a?.uGmiPercent.map { String(format: "%.1f%%", $0) } ?? "--",
                       sub: a1cSub(a), exact: true)
                metric("Avg", a?.meanMgdl.map { GlucoseValue(mgdl: $0).display(in: unit) } ?? "--",
                       sub: unit.label)
                metric("In range", a.map { String(format: "%.0f%%", $0.inRangePct) } ?? "--",
                       sub: "70–180")
                metric("CV", a?.cvPercent.map { String(format: "%.0f%%", $0) } ?? "--",
                       sub: a?.cvPercent.map { $0 <= 36 ? "stable" : "variable" } ?? " ")
            }
            if let a { TIRBar(analytics: a) }
        }
        .padding(14)
        .background(Color.nkTile, in: RoundedRectangle(cornerRadius: 18))
    }

    private var chartCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("GLUCOSE").font(.caption2).bold().foregroundStyle(.secondary)
                Spacer()
                if let c = model.current {
                    Text(c.date, style: .relative).font(.caption2).foregroundStyle(.secondary)
                }
            }
            GlucoseChartView(readings: model.readings, unit: unit,
                             lowMgdl: model.settings.lowThresholdMgdl,
                             highMgdl: model.settings.highThresholdMgdl)
                .frame(maxHeight: .infinity)
        }
        .padding(14)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.nkTile, in: RoundedRectangle(cornerRadius: 18))
    }

    /// The trailing-summary sub for the uGMI tile: names the other two A1c estimates so
    /// the reader always knows which is which (e.g. "GMI 6.0 · eA1c 5.6").
    private func a1cSub(_ a: GlucoseAnalytics?) -> String {
        guard let a else { return " " }
        var parts: [String] = []
        if let g = a.gmiPercent { parts.append(String(format: "GMI %.1f", g)) }
        if let e = a.estimatedA1cPercent { parts.append(String(format: "eA1c %.1f", e)) }
        return parts.isEmpty ? " " : parts.joined(separator: " · ")
    }

    /// `exact` keeps the label's own casing — used for "uGMI", where the lowercase "u"
    /// distinguishes it from GMI and is the whole point of the label.
    private func metric(_ label: String, _ value: String, sub: String, exact: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(exact ? label : label.uppercased()).font(.caption2).foregroundStyle(.secondary)
            Text(value).font(.system(.title2, design: .rounded)).bold()
                .lineLimit(1).minimumScaleFactor(0.7)
            Text(sub).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(Color.nkInk, in: RoundedRectangle(cornerRadius: 10))
    }
}

/// A compact last-hour sparkline for the spotlight reading: a soft filled line with an
/// endpoint dot. The time axis is pinned to a fixed **now−1h … now** window (so the right
/// edge is the latest reading and the left edge is exactly one hour ago), and faint
/// 15-minute guides with a tiny "−1h" anchor make that span legible without stealing
/// focus from the big number. Mirrors the widget's `TrendSparkline` (which lives in the
/// widget target and can't be shared without project surgery).
struct MiniSparkline: View {
    let readings: [GlucoseReading]
    let color: Color

    var body: some View {
        GeometryReader { geo in
            let now = Date()
            let start = now.addingTimeInterval(-3600)
            let pts = Self.points(readings, size: geo.size, start: start, end: now)
            ZStack(alignment: .bottomLeading) {
                // Subtle quarter-hour guides; the left edge (−1h) is a touch stronger so
                // the eye reads "this is the last hour" at a glance.
                ForEach([60, 45, 30, 15], id: \.self) { mins in
                    Rectangle()
                        .fill(Color.nkMuted.opacity(mins == 60 ? 0.22 : 0.09))
                        .frame(width: 1, height: geo.size.height)
                        .position(x: geo.size.width * CGFloat(1 - Double(mins) / 60.0),
                                  y: geo.size.height / 2)
                }
                if pts.count >= 2 {
                    // The smoothed line + fill, faded in from the left so it emerges rather
                    // than starting at a hard vertical edge (the dot stays full-strength).
                    ZStack {
                        fillPath(pts, height: geo.size.height)
                            .fill(LinearGradient(colors: [color.opacity(0.28), color.opacity(0.0)],
                                                 startPoint: .top, endPoint: .bottom))
                        smoothLine(pts)
                            .stroke(color, style: StrokeStyle(lineWidth: 2.2, lineCap: .round, lineJoin: .round))
                    }
                    .mask(LinearGradient(stops: [
                        .init(color: .clear, location: 0),
                        .init(color: .black, location: 0.18),
                        .init(color: .black, location: 1),
                    ], startPoint: .leading, endPoint: .trailing))
                    if let last = pts.last {
                        Circle().fill(color).frame(width: 5, height: 5).position(last)
                    }
                }
                // The quiet axis hint: this graph covers the trailing hour.
                Text("−1h")
                    .font(.system(size: 8, weight: .medium))
                    .foregroundStyle(Color.nkMuted.opacity(0.55))
                    .padding(.leading, 1).padding(.bottom, -1)
            }
        }
        .accessibilityHidden(true)
    }

    /// Catmull-Rom → cubic-bézier smoothing (the same non-overshooting curve the web spark
    /// and the main chart use), so the line reads as a calm sweep rather than a scribble.
    private func smoothLine(_ pts: [CGPoint]) -> Path {
        var p = Path()
        guard let first = pts.first else { return p }
        p.move(to: first)
        guard pts.count >= 3 else {
            for q in pts.dropFirst() { p.addLine(to: q) }
            return p
        }
        for i in 0..<pts.count - 1 {
            let p0 = i > 0 ? pts[i - 1] : pts[i]
            let p1 = pts[i]
            let p2 = pts[i + 1]
            let p3 = i + 2 < pts.count ? pts[i + 2] : p2
            let c1 = CGPoint(x: p1.x + (p2.x - p0.x) / 6, y: p1.y + (p2.y - p0.y) / 6)
            let c2 = CGPoint(x: p2.x - (p3.x - p1.x) / 6, y: p2.y - (p3.y - p1.y) / 6)
            p.addCurve(to: p2, control1: c1, control2: c2)
        }
        return p
    }

    private func fillPath(_ pts: [CGPoint], height: CGFloat) -> Path {
        var p = smoothLine(pts)
        if let first = pts.first, let last = pts.last {
            p.addLine(to: CGPoint(x: last.x, y: height))
            p.addLine(to: CGPoint(x: first.x, y: height))
            p.closeSubpath()
        }
        return p
    }

    /// Map readings to points over a fixed [start, end] window, so x position encodes the
    /// real time-of-reading (the left edge is `start` = one hour ago, not the first point).
    /// Values are smoothed first so a noisy 1-min stream reads as a calm trend.
    static func points(_ readings: [GlucoseReading], size: CGSize, start: Date, end: Date) -> [CGPoint] {
        let recent = readings.filter { $0.date >= start }.sorted { $0.date < $1.date }
        guard recent.count >= 2 else { return [] }
        // Window scales with sample density (~1 per 12 points), forced odd; it narrows at
        // the ends so the final point stays faithful to the latest reading.
        let raw = max(3, recent.count / 12)
        let win = raw.isMultiple(of: 2) ? raw + 1 : raw
        let vals = movingAverage(recent.map(\.mgdl), window: win)
        let lo = (vals.min() ?? 80) - 6
        let hi = (vals.max() ?? 180) + 6
        let span = max(1, hi - lo)
        let t0 = start.timeIntervalSince1970
        let dt = max(1, end.timeIntervalSince1970 - t0)
        let pad: CGFloat = 4
        let h = max(1, size.height - pad * 2)
        return zip(recent, vals).map { r, v in
            CGPoint(x: CGFloat((r.date.timeIntervalSince1970 - t0) / dt) * size.width,
                    y: pad + CGFloat(1 - (v - lo) / span) * h)
        }
    }

    /// Centred moving average (window narrows at the edges).
    static func movingAverage(_ vals: [Double], window: Int) -> [Double] {
        let n = vals.count, half = window / 2
        return vals.indices.map { i in
            let lo = Swift.max(0, i - half), hi = Swift.min(n - 1, i + half)
            return vals[lo...hi].reduce(0, +) / Double(hi - lo + 1)
        }
    }
}

/// The NightKnight brand glyph — a guard's shield with a glucose trace and the single
/// red current-reading marker — drawn to match the app icon / favicon. Scales crisply.
struct NightKnightLogo: View {
    var height: CGFloat = 28

    var body: some View {
        Canvas { ctx, size in
            let s = size.height / 512.0
            let line = Color(red: 0.957, green: 0.961, blue: 0.965)
            func pt(_ x: Double, _ y: Double) -> CGPoint { CGPoint(x: x * s, y: y * s) }

            var shield = Path()
            shield.move(to: pt(256, 120))
            shield.addLine(to: pt(360, 152))
            shield.addLine(to: pt(360, 250))
            shield.addCurve(to: pt(256, 398), control1: pt(360, 322), control2: pt(312, 372))
            shield.addCurve(to: pt(152, 250), control1: pt(200, 372), control2: pt(152, 322))
            shield.addLine(to: pt(152, 152))
            shield.closeSubpath()
            ctx.stroke(shield, with: .color(line), style: StrokeStyle(lineWidth: 24 * s, lineCap: .round, lineJoin: .round))

            var trace = Path()
            trace.move(to: pt(182, 268))
            trace.addLine(to: pt(212, 250))
            trace.addLine(to: pt(240, 282))
            trace.addLine(to: pt(270, 244))
            trace.addLine(to: pt(300, 262))
            ctx.stroke(trace, with: .color(line), style: StrokeStyle(lineWidth: 18 * s, lineCap: .round, lineJoin: .round))

            let r = 17.0 * s
            ctx.fill(Path(ellipseIn: CGRect(x: 324 * s - r, y: 240 * s - r, width: r * 2, height: r * 2)),
                     with: .color(Color.nkAccent))
        }
        // The glyph is square; size by height and keep a 1:1 aspect.
        .frame(width: height, height: height)
    }
}

// MARK: - Analysis

/// Plain-language explanations surfaced through the (?) info buttons — opened as a
/// readable modal sheet. Each explains what the metric is, how it's computed, what a
/// good value looks like, and why it matters.
/// A metric explanation plus a link to a reputable source to read more.
struct TipInfo {
    /// Where the "Learn more" link in the modal points — a primary clinical reference or
    /// a trusted patient-facing explainer for this metric.
    let url: String
    let text: String
}

private enum Tip {
    static let gri = TipInfo(url: "https://pmc.ncbi.nlm.nih.gov/articles/PMC10563532/", text: """
    The Glycemia Risk Index is a single 0–100 score (lower is better) that blends your \
    low- and high-glucose risk into one number, weighted the way a panel of 330 \
    clinicians ranked severity.

    GRI = 3.0 × (time below 54 + 0.8 × time 54–69) + 1.6 × (time above 250 + 0.5 × time \
    181–250), capped at 100. Lows are weighted more heavily than highs because they are \
    more immediately dangerous.

    Zones run A (0–20, best) through E (80–100, highest risk). The GRI captures extreme \
    excursions that a plain time-in-range number can miss. (Klonoff et al., 2023.)
    """)
    static let mean = TipInfo(url: "https://pmc.ncbi.nlm.nih.gov/articles/PMC6973648/", text: """
    Your average sensor glucose across the selected period, shown in your display unit.

    It's the foundation for the A1c estimates (GMI and eA1c). There's no single ideal \
    mean — a lower average usually maps to a lower estimated A1c, but it has to be \
    balanced against how much time you spend low. Read it alongside Time in Range and \
    variability rather than on its own.
    """)
    static let ugmi = TipInfo(url: "https://doi.org/10.1007/s00125-026-06739-w", text: """
    Updated GMI (uGMI) is the 2026 revision of the Glucose Management Indicator. It \
    estimates a lab A1c from your average sensor glucose, but its model aligns more \
    closely with measured HbA1c than the original 2018 GMI — most noticeably at lower \
    averages, where the old formula tended to read high.

    uGMI(%) = 1 / (15.36 / mean glucose (mg/dL) + 0.0425).

    NightKnight leads with uGMI everywhere and shows the 2018 GMI and the older eA1c \
    alongside it for comparison. Like any estimate it needs at least 14 days with more \
    than 70% sensor-active data, and a real lab A1c can still differ by up to about 1% \
    because A1c also depends on red-blood-cell lifespan and other personal factors. \
    (Bergenstal et al., Diabetologia 2026.)
    """)
    static let gmi = TipInfo(url: "https://pubmed.ncbi.nlm.nih.gov/30224348/", text: """
    The Glucose Management Indicator (2018) estimates what a lab A1c would be from your \
    average sensor glucose.

    GMI(%) = 3.31 + 0.02392 × mean glucose (mg/dL) — a higher average gives a higher GMI.

    It's shown here for comparison; NightKnight prefers the updated uGMI, which realigns \
    the estimate to better match measured HbA1c. It's only reliable over at least 14 days \
    with more than 70% sensor-active data, and GMI and an actual lab A1c can still differ \
    by up to about 1% in either direction, because A1c also depends on red-blood-cell \
    lifespan and other personal factors. (Bergenstal et al., 2018.)
    """)
    static let ea1c = TipInfo(url: "https://pubmed.ncbi.nlm.nih.gov/?term=translating+the+A1C+assay+into+estimated+average+glucose", text: """
    An older estimate of A1c from your average glucose, using the 2008 ADAG regression:

    eA1c(%) = (mean mg/dL + 46.7) ÷ 28.7.

    It's kept for compatibility with tools that report it, but it can diverge from GMI — \
    the two are fit on different populations and use different slopes. When they \
    disagree, prefer GMI.
    """)
    static let sd = TipInfo(url: "https://pmc.ncbi.nlm.nih.gov/articles/PMC6973648/", text: """
    Standard deviation is the absolute spread of your glucose around its mean, in mg/dL \
    (or mmol/L). A lower SD means steadier glucose.

    Because SD scales with the average, it's most useful read together with CV (which \
    divides SD by the mean). It's computed with the sample (N−1) formula, matching the \
    standard CGM-analysis tools.
    """)
    static let cv = TipInfo(url: "https://pmc.ncbi.nlm.nih.gov/articles/PMC6973648/", text: """
    The coefficient of variation is the standard measure of glucose stability:

    CV(%) = standard deviation ÷ mean × 100.

    Dividing by the mean makes it comparable across people and periods. The international \
    consensus target is ≤ 36% — at or below this, glucose is considered stable; above it, \
    more labile and more prone to lows. Many clinicians aim for < 33% on insulin or \
    sulfonylureas.
    """)
    static let active = TipInfo(url: "https://pmc.ncbi.nlm.nih.gov/articles/PMC6973648/", text: """
    The percentage of the selected period that actually has CGM data — readings present \
    ÷ readings expected at a 5-minute cadence (288 per day).

    It tells you how much to trust everything else on this page. The consensus is that \
    the metrics are reliable over at least 14 days with more than 70% active data; below \
    that they're flagged as limited. A low value usually means sensor gaps or warm-up time.
    """)
    static let agp = TipInfo(url: "https://diatribe.org/diabetes-technology/making-most-cgm-uncover-magic-your-ambulatory-glucose-profile", text: """
    The Ambulatory Glucose Profile overlays every day in the period onto a single \
    24-hour day, so you can see your typical daily pattern at a glance.

    The line is the median (the middle reading at each time of day). The inner band is \
    the middle 50% of readings (25th–75th percentile) and the outer band the middle 90% \
    (5th–95th). The dashed lines mark your target range.

    A tight band sitting inside the target range is the goal; a wide band means high \
    day-to-day variability at that time, and a band drifting low or high shows when \
    problems cluster. It's the single most informative picture in diabetes care. \
    (Bergenstal AGP.)
    """)
    static let tod = TipInfo(url: "https://pmc.ncbi.nlm.nih.gov/articles/PMC6973648/", text: """
    Your average glucose and time-in-range for each quarter of the day — overnight \
    (00–06), morning (06–12), afternoon (12–18) and evening (18–24), in your local time.

    Splitting the day this way surfaces patterns a single average hides: overnight lows, \
    the dawn rise, or post-meal highs. Compare the four to find which part of your day \
    needs the most attention.
    """)
    static let episodes = TipInfo(url: "https://pmc.ncbi.nlm.nih.gov/articles/PMC6973648/", text: """
    A hypo- or hyper-glycemic event is a stretch of at least 15 minutes beyond a \
    threshold, considered over once glucose is back across it for at least 15 minutes \
    (the 2019 international consensus definition).

    Lows use 70 and 54 mg/dL; highs use 180 and 250. Where Time in Range says how much \
    time you spend out of range, episodes say how often and how long — which is what you \
    actually act on. "Nocturnal" counts lows that begin between 00:00 and 06:00. Brief \
    blips back into range don't split one event in two, and sensor gaps aren't bridged \
    into a single event.
    """)
    static let advanced = TipInfo(url: "https://pmc.ncbi.nlm.nih.gov/articles/PMC6973648/", text: """
    Deeper glucose-variability indices for when you want more than CV. Each tile has its \
    own (?) explaining exactly what it measures and how to read it. They're estimates for \
    personal insight, not treatment targets.
    """)
    static let jindex = TipInfo(url: "https://pubmed.ncbi.nlm.nih.gov/?term=J-index+glycaemic+variability", text: """
    The J-index combines your average and your variability into one severity score:

    J = 0.001 × (mean + SD)², with both in mg/dL.

    It rises when either the average or the spread is high, so it penalises being high \
    AND being erratic. A non-diabetic reference range is roughly 4.7–23.6; lower is \
    better. (Wojcicki, 1995.)
    """)
    static let mage = TipInfo(url: "https://pubmed.ncbi.nlm.nih.gov/?term=mean+amplitude+of+glycemic+excursions", text: """
    Mean Amplitude of Glycemic Excursions measures the size of your meaningful swings.

    It finds the peak-to-trough amplitudes between turning points and averages only those \
    larger than 1 standard deviation — so small wobble is ignored and the big rises and \
    falls you actually feel are captured. A smaller MAGE means gentler swings. \
    (Service, 1970; the metric is known to be algorithm-sensitive.)
    """)
    static let conga = TipInfo(url: "https://pubmed.ncbi.nlm.nih.gov/?term=continuous+overall+net+glycemic+action+CONGA", text: """
    CONGA — Continuous Overall Net Glycemic Action — measures within-day variability.

    It takes the difference between each reading and the reading n hours earlier (2 hours \
    here) and reports the spread (standard deviation) of those differences. A smaller \
    CONGA means glucose changes less over that window — i.e. steadier within the day. \
    (McDonnell, 2005.)
    """)
}

/// A tappable "?" that opens the metric's full explanation as a readable modal sheet.
struct InfoTip: View {
    let title: String
    let info: TipInfo
    @State private var show = false
    init(_ title: String, _ info: TipInfo) { self.title = title; self.info = info }
    var body: some View {
        Button { show = true } label: {
            Image(systemName: "questionmark.circle").font(.caption2).foregroundStyle(.secondary)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("About \(title)")
        .sheet(isPresented: $show) { InfoSheet(title: title, info: info) }
    }
}

/// The explanation modal: a titled, scrollable sheet you can read in full, with a link
/// out to a reputable source to learn more, then dismiss.
struct InfoSheet: View {
    let title: String
    let info: TipInfo
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    Text(info.text)
                        .font(.body)
                        .lineSpacing(3)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .textSelection(.enabled)
                    if let url = URL(string: info.url) {
                        Link(destination: url) {
                            Label("Learn more", systemImage: "arrow.up.right.square")
                                .font(.callout.weight(.semibold))
                        }
                        .tint(Color.nkAccent)
                    }
                }
                .padding(20)
            }
            .background(Color.nkInk)
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .tint(Color.nkAccent)
    }
}

@MainActor
@Observable
final class AnalysisModel {
    var analytics: GlucoseAnalytics?
    var agp: [AgpBin] = []
    var period = 14
    var errorText: String?
    let settings = Settings.shared
    private var client: APIClient { APIClient(settings: settings) }

    func load() async {
        do {
            analytics = try await client.analytics(hours: period * 24)
            errorText = nil
        } catch {
            errorText = (error as? APIError)?.errorDescription ?? error.localizedDescription
            return
        }
        // AGP is best-effort: a server without the /agp endpoint must not blank the page.
        agp = (try? await client.agp(days: period)) ?? []
    }
}

/// The Statistical-Analysis view — GRI, core metrics, AGP, time-of-day, episodes and
/// advanced variability, each with a (?) explanation. Mirrors the web Analysis page.
struct AnalysisView: View {
    @State private var model = AnalysisModel()
    /// The export file currently being shared (drives the share sheet); nil = not sharing.
    @State private var shareItem: ReportShareItem?
    private let cols = [GridItem(.flexible(), spacing: 12), GridItem(.flexible(), spacing: 12)]
    private var unit: GlucoseUnit { model.settings.preferredUnit }
    private var lowMgdl: Double { model.settings.lowThresholdMgdl }
    private var highMgdl: Double { model.settings.highThresholdMgdl }

    var body: some View {
        NavigationStack {
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        Picker("Period", selection: $model.period) {
                            Text("7d").tag(7); Text("14d").tag(14); Text("30d").tag(30)
                        }
                        .pickerStyle(.segmented)
                        .onChange(of: model.period) { Task { await model.load() } }

                        if let a = model.analytics {
                            Text(caption(a)).font(.caption).foregroundStyle(.secondary)
                            griCard(a.gri).id("gri")
                            coreMetrics(a).id("core")
                            agpCard().id("agp")
                            timeOfDay(a.patterns).id("tod")
                            episodesCard(a.episodes).id("episodes")
                            advancedCard(a.variability, sd: a.sdMgdl).id("advanced")
                            Text("Not a medical device. These metrics are estimates for personal insight, not a basis for treatment decisions.")
                                .font(.caption2).foregroundStyle(.secondary).padding(.top, 4)
                        } else if let err = model.errorText {
                            Text(err).font(.callout).foregroundStyle(Color.nkAccent).padding(.top, 40)
                        } else {
                            ProgressView().frame(maxWidth: .infinity).padding(.top, 60)
                        }
                    }
                    .padding()
                }
                #if DEBUG
                // Screenshot helper: jump to a named section once data is in.
                .onChange(of: model.analytics == nil) { _, isNil in
                    if !isNil, let t = Demo.scrollTarget {
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
                            withAnimation { proxy.scrollTo(t, anchor: .top) }
                        }
                    }
                }
                #endif
            }
            .background(Color.nkInk.ignoresSafeArea())
            .navigationTitle("Analysis")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Button { exportPDF() } label: { Label("AGP report (PDF)", systemImage: "doc.richtext") }
                        Button { exportCSV() } label: { Label("Readings (CSV)", systemImage: "tablecells") }
                        Button { exportJSON() } label: { Label("Metrics (JSON)", systemImage: "curlybraces") }
                    } label: {
                        Image(systemName: "square.and.arrow.up")
                    }
                    .disabled(model.analytics == nil)
                }
            }
            .sheet(item: $shareItem) { item in ReportShareSheet(items: [item.url]) }
            .task { if model.analytics == nil { await model.load() } }
            #if DEBUG
            .task { await autoplaySweep() }
            #endif
            .refreshable { await model.load() }
        }
        .tint(Color.nkAccent)
    }

    #if DEBUG
    /// Preview recording: sweep the period selector so the cards animate.
    private func autoplaySweep() async {
        guard Demo.autoplay else { return }
        try? await Task.sleep(for: .seconds(2))
        let seq = [7, 14, 30]
        while !Task.isCancelled {
            for p in seq {
                withAnimation { model.period = p }
                try? await Task.sleep(for: .seconds(2.0))
            }
        }
    }
    #endif

    // MARK: cards

    private func griCard(_ gri: GriInfo) -> some View {
        let c = zoneColor(gri.zone)
        return card("Glycemia Risk Index", tip: Tip.gri) {
            HStack(alignment: .bottom, spacing: 12) {
                Text(gri.value == nil ? "--" : String(format: "%.0f", gri.value!))
                    .font(.system(size: 54, weight: .bold, design: .rounded)).foregroundStyle(c)
                HStack(spacing: 6) {
                    Circle().fill(c).frame(width: 8, height: 8)
                    Text("Zone \(gri.zone ?? "—")").font(.subheadline).bold()
                }
                .padding(.horizontal, 12).padding(.vertical, 6)
                .background(Color.nkInk, in: Capsule())
                .padding(.bottom, 8)
                Spacer()
            }
            Text("0 = lowest risk · lower is better").font(.caption).foregroundStyle(.secondary)
            griComp("Hypoglycemia", gri.hypoComponent, color: .nkWarn)
            griComp("Hyperglycemia", gri.hyperComponent, color: .nkDanger)
        }
    }

    private func coreMetrics(_ a: GlucoseAnalytics) -> some View {
        card("Core Metrics") {
            LazyVGrid(columns: cols, spacing: 12) {
                metricTile("Mean Glucose", fmtGlu(a.meanMgdl), suffix: unit.label, sub: "\(a.n) readings", tip: Tip.mean)
                metricTile("uGMI", num(a.uGmiPercent, 1), suffix: "%", sub: "updated · preferred", tip: Tip.ugmi, exact: true)
                metricTile("GMI", num(a.gmiPercent, 1), suffix: "%", sub: "2018 estimate", tip: Tip.gmi, exact: true)
                metricTile("eA1c", num(a.estimatedA1cPercent, 1), suffix: "%", sub: "ADAG (legacy)", tip: Tip.ea1c, exact: true)
                metricTile("SD", fmtGlu(a.sdMgdl), suffix: unit.label, sub: "std deviation", tip: Tip.sd)
                metricTile("CV", num(a.cvPercent, 0), suffix: "%", sub: cvNote(a.cvPercent), tip: Tip.cv)
                metricTile("Time Active", num(a.coverage.percentActive, 0), suffix: "%", sub: a.coverage.sufficient ? "sufficient" : "limited", tip: Tip.active)
            }
        }
    }

    private func agpCard() -> some View {
        card("Ambulatory Glucose Profile", tip: Tip.agp) {
            AGPChartView(bins: model.agp, unit: unit, lowMgdl: lowMgdl, highMgdl: highMgdl)
            HStack(spacing: 16) {
                legend(Color(white: 0.95), "Median", line: true)
                legend(Color.nkInRange.opacity(0.55), "25–75%")
                legend(Color.nkInRange.opacity(0.28), "5–95%")
            }
            .font(.caption2).foregroundStyle(.secondary)
        }
    }

    private func timeOfDay(_ patterns: [PeriodInfo]) -> some View {
        let names = ["Overnight", "Morning", "Afternoon", "Evening"]
        return card("Time-of-Day Patterns", tip: Tip.tod) {
            LazyVGrid(columns: cols, spacing: 12) {
                ForEach(Array(patterns.enumerated()), id: \.offset) { i, p in
                    let label = "\(i < names.count ? names[i] : "") \(String(format: "%02d–%02d", p.startHour, p.endHour))"
                    metricTile(label, fmtGlu(p.meanMgdl), suffix: unit.label,
                               sub: p.meanMgdl == nil ? "no data" : "\(num(p.inRangePct, 0))% in range")
                }
            }
        }
    }

    private func episodesCard(_ ep: EpisodesInfo) -> some View {
        card("Episodes", tip: Tip.episodes) {
            HStack(alignment: .top) {
                epStat("\(ep.low.count)", "low · \(String(format: "%.1f", ep.low.perDay))/day", .nkWarn)
                epStat("\(ep.low.nocturnal)", "nocturnal", Color(red: 0.43, green: 0.55, blue: 1.0))
                epStat("\(ep.high.count)", "high · \(String(format: "%.1f", ep.high.perDay))/day", .nkDanger)
            }
            Text("Longest low \(fmtDur(ep.low.longestMin)) · \(ep.veryLow.count) severe (<54)")
                .font(.caption).foregroundStyle(.secondary)
            VStack(spacing: 8) {
                ForEach(ep.recent) { e in
                    HStack(spacing: 10) {
                        Circle().fill(e.kind == "low" ? Color.nkWarn : Color.nkDanger).frame(width: 8, height: 8)
                        Text(e.kind == "low" ? "Low" : "High").font(.caption).bold().frame(width: 40, alignment: .leading)
                        Text(e.start, format: .dateTime.weekday().hour().minute()).font(.caption).foregroundStyle(.secondary)
                        Spacer()
                        Text("\(e.kind == "low" ? "down to" : "up to") \(fmtGlu(e.extremeMgdl)) · \(fmtDur(e.durationMin))")
                            .font(.caption2).foregroundStyle(.secondary)
                    }
                    .padding(.vertical, 8).padding(.horizontal, 12)
                    .background(Color.nkInk, in: RoundedRectangle(cornerRadius: 10))
                }
                if ep.recent.isEmpty {
                    Text("No episodes in this period — nice.").font(.caption).foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }

    private func advancedCard(_ v: VariabilityInfo, sd: Double?) -> some View {
        card("Advanced Variability", tip: Tip.advanced) {
            LazyVGrid(columns: cols, spacing: 12) {
                metricTile("J-Index", num(v.jIndex, 1), sub: "mean + SD severity", tip: Tip.jindex)
                metricTile("MAGE", fmtGlu(v.mage), suffix: unit.label, sub: "mean large swing", tip: Tip.mage)
                metricTile("CONGA-\(Int(v.congaHours ?? 2))h", fmtGlu(v.conga), suffix: unit.label, sub: "within-day variability", tip: Tip.conga)
                metricTile("SD", fmtGlu(sd), suffix: unit.label, sub: "absolute spread", tip: Tip.sd)
            }
        }
    }

    // MARK: building blocks

    private func card<C: View>(_ title: String, tip: TipInfo? = nil, @ViewBuilder content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 5) {
                Text(title.uppercased()).font(.caption).bold().foregroundStyle(.secondary)
                if let tip { InfoTip(title, tip) }
            }
            content()
        }
        .padding(20)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.nkTile, in: RoundedRectangle(cornerRadius: 20))
    }

    private func metricTile(_ label: String, _ value: String, suffix: String = "", sub: String, tip: TipInfo? = nil, exact: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 4) {
                Text(exact ? label : label.uppercased()).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                if let tip { InfoTip(label, tip) }
            }
            HStack(alignment: .firstTextBaseline, spacing: 2) {
                Text(value).font(.system(.title2, design: .rounded)).bold()
                if !suffix.isEmpty { Text(suffix).font(.caption2).foregroundStyle(.secondary) }
            }
            Text(sub).font(.caption2).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(Color.nkInk, in: RoundedRectangle(cornerRadius: 12))
    }

    private func griComp(_ label: String, _ value: Double?, color: Color) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack {
                Text(label).font(.caption).foregroundStyle(.secondary)
                Spacer()
                Text(value == nil ? "—" : String(format: "%.1f", value!)).font(.caption).bold().foregroundStyle(color)
            }
            GeometryReader { geo in
                ZStack(alignment: .leading) {
                    Capsule().fill(Color.nkInk)
                    Capsule().fill(color).frame(width: geo.size.width * min(1.0, (value ?? 0) / 40.0))
                }
            }
            .frame(height: 7)
        }
    }

    private func epStat(_ n: String, _ cap: String, _ color: Color) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(n).font(.system(.title2, design: .rounded)).bold().foregroundStyle(color)
            Text(cap).font(.caption2).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func legend(_ color: Color, _ label: String, line: Bool = false) -> some View {
        HStack(spacing: 6) {
            if line {
                Rectangle().fill(color).frame(width: 14, height: 2)
            } else {
                RoundedRectangle(cornerRadius: 2).fill(color).frame(width: 14, height: 10)
            }
            Text(label)
        }
    }

    // MARK: formatting

    private func fmtGlu(_ mgdl: Double?) -> String {
        guard let m = mgdl else { return "--" }
        return GlucoseValue(mgdl: m).display(in: unit)
    }
    private func num(_ v: Double?, _ digits: Int) -> String { v == nil ? "--" : String(format: "%.\(digits)f", v!) }
    private func cvNote(_ cv: Double?) -> String { cv == nil ? "—" : cv! <= 36 ? "stable" : "unstable" }
    private func fmtDur(_ min: Double) -> String { let m = Int(min.rounded()); return m >= 60 ? "\(m / 60)h \(m % 60)m" : "\(m)m" }
    private func caption(_ a: GlucoseAnalytics) -> String {
        let active = a.coverage.percentActive.map { String(format: "%.0f%% active", $0) } ?? "—"
        let suffix = a.coverage.sufficient ? "" : " · limited data"
        return "Based on \(a.n) readings over \(model.period) days · \(active)\(suffix)"
    }
    private func zoneColor(_ zone: String?) -> Color {
        switch zone {
        case "A": return .nkInRange
        case "B": return Color(red: 0.61, green: 0.83, blue: 0.29)
        case "C": return Color(red: 0.90, green: 0.79, blue: 0.24)
        case "D": return .nkWarn
        case "E": return .nkDanger
        default: return .nkMuted
        }
    }

    // MARK: export & report

    /// The export window is the currently-selected analysis period (7/14/30 days), ending now.
    private func exportRange() -> ExportRange { ExportRange.trailing(days: model.period) }

    /// Render the light-themed AGP one-pager to a PDF and present the share sheet. Works in
    /// every mode, including standalone (the report is built from on-device analytics).
    @MainActor
    private func exportPDF() {
        guard let a = model.analytics else { return }
        let report = AGPReportView(analytics: a, agp: model.agp, range: exportRange(),
                                   unit: unit, lowMgdl: lowMgdl, highMgdl: highMgdl)
        if let url = ReportPDF.render(report, fileName: "NightKnight-AGP-\(model.period)d.pdf") {
            shareItem = ReportShareItem(url: url)
        }
    }

    /// Export the full computed metric set as JSON (built from the loaded analytics + AGP).
    @MainActor
    private func exportJSON() {
        guard let a = model.analytics else { return }
        let data = GlucoseExport.metricsJSON(analytics: a, agp: model.agp, range: exportRange())
        if let url = ExportFile.write(data, name: "NightKnight-metrics-\(model.period)d.json") {
            shareItem = ReportShareItem(url: url)
        }
    }

    /// Export the raw readings over the period as CSV. Readings aren't held by the Analysis
    /// model, so fetch them (server or on-device) before building the file.
    @MainActor
    private func exportCSV() {
        let period = model.period
        let settings = model.settings
        Task { @MainActor in
            let readings = (try? await APIClient(settings: settings).entries(hours: period * 24)) ?? []
            let csv = GlucoseExport.readingsCSV(readings, range: ExportRange.trailing(days: period))
            if let url = ExportFile.write(csv, name: "NightKnight-readings-\(period)d.csv") {
                shareItem = ReportShareItem(url: url)
            }
        }
    }
}

/// Stacked time-in-range bar.
private struct TIRBar: View {
    let analytics: GlucoseAnalytics
    var body: some View {
        let segs: [(Double, Color)] = [
            (analytics.veryLowPct, .nkDanger), (analytics.lowPct, .nkWarn),
            (analytics.inRangePct, .nkInRange), (analytics.highPct, .nkWarn),
            (analytics.veryHighPct, .nkDanger),
        ]
        GeometryReader { geo in
            HStack(spacing: 0) {
                ForEach(Array(segs.enumerated()), id: \.offset) { _, s in
                    Rectangle().fill(s.1).frame(width: geo.size.width * s.0 / 100)
                }
            }
        }
        .frame(height: 12).clipShape(RoundedRectangle(cornerRadius: 6))
    }
}
