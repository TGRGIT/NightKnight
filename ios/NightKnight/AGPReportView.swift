import SwiftUI
import Charts
import UIKit

/// A light-themed, print-ready Ambulatory Glucose Profile one-pager rendered natively on
/// device — so a standalone source (Dexcom Share / LibreLinkUp / Nightscout, no NightKnight
/// server) can produce the same clinical report as server mode. Composed from the analytics
/// and AGP the Analysis view already holds, then rasterised to a PDF via [`ReportPDF`] and
/// shared. Deliberately light (dark ink on white) because the app ships dark but a report
/// prints on paper.
struct AGPReportView: View {
    let analytics: GlucoseAnalytics
    let agp: [AgpBin]
    let range: ExportRange
    let unit: GlucoseUnit
    let lowMgdl: Double
    let highMgdl: Double

    // Clinical palette (dark-red low → green target → orange very-high), on white.
    private static let paper = Color.white
    private static let ink = Color(red: 0.075, green: 0.090, blue: 0.125)
    private static let muted = Color(red: 0.36, green: 0.40, blue: 0.46)
    private static let faint = Color(red: 0.54, green: 0.58, blue: 0.64)
    private static let rim = Color(red: 0.86, green: 0.88, blue: 0.91)
    private static let accent = Color(red: 0.898, green: 0.282, blue: 0.302)
    private static let bVLow = Color(red: 0.63, green: 0.12, blue: 0.12)
    private static let bLow = Color(red: 0.86, green: 0.32, blue: 0.27)
    private static let bInRange = Color(red: 0.184, green: 0.62, blue: 0.34)
    private static let bHigh = Color(red: 0.95, green: 0.76, blue: 0.31)
    private static let bVHigh = Color(red: 0.88, green: 0.55, blue: 0.18)

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            header
            Rectangle().fill(Self.ink).frame(height: 2)
            HStack(alignment: .top, spacing: 20) {
                statistics.frame(width: 250)
                tirGoalBar
            }
            sectionTitle("Ambulatory Glucose Profile (AGP)")
            ReportAGPChart(bins: agp, unit: unit, lowMgdl: lowMgdl, highMgdl: highMgdl)
                .frame(height: 230)
                .padding(12)
                .background(RoundedRectangle(cornerRadius: 10).stroke(Self.rim))
            Spacer(minLength: 0)
            footer
        }
        .padding(28)
        .frame(width: 595, height: 842, alignment: .top)
        .background(Self.paper)
        .foregroundStyle(Self.ink)
    }

    // MARK: header

    private var header: some View {
        HStack(alignment: .top) {
            HStack(spacing: 10) {
                // The glyph's strokes are near-white (built for the dark app), so seat it on
                // the brand-red chip — the same red-square-with-crescent mark the web report uses.
                NightKnightLogo(height: 24)
                    .frame(width: 34, height: 34)
                    .background(Self.accent, in: RoundedRectangle(cornerRadius: 9))
                VStack(alignment: .leading, spacing: 2) {
                    Text("Ambulatory Glucose Profile (AGP) Report")
                        .font(.system(size: 17, weight: .bold))
                    Text("NightKnight — continuous glucose monitoring summary")
                        .font(.system(size: 11)).foregroundStyle(Self.muted)
                }
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 2) {
                Text(rangeText).font(.system(size: 12, weight: .semibold))
                Text("\(range.days) day report period").font(.system(size: 11)).foregroundStyle(Self.muted)
                Text("Generated \(generatedText)").font(.system(size: 11)).foregroundStyle(Self.muted)
            }
        }
    }

    // MARK: statistics

    private var statistics: some View {
        VStack(alignment: .leading, spacing: 10) {
            sectionTitle("Glucose Statistics")
            statTile("% Time CGM Active", pct(analytics.coverage.percentActive) + "%",
                     "\(analytics.n) readings")
            statTile("Average Glucose", "\(fmtGlu(analytics.meanMgdl)) \(unit.label)",
                     "SD \(fmtGlu(analytics.sdMgdl))")
            statTile("Glucose Mgmt Indicator", pct(analytics.uGmiPercent, 1) + "%",
                     "uGMI · GMI \(pct(analytics.gmiPercent, 1))%")
            statTile("Variability (CV)", pct(analytics.cvPercent) + "%",
                     analytics.cvPercent.map { $0 <= 36 ? "stable (≤36%)" : "elevated" } ?? "—")
            griTile
        }
    }

    private func statTile(_ k: String, _ v: String, _ s: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(k.uppercased()).font(.system(size: 9, weight: .semibold)).foregroundStyle(Self.faint)
            Text(v).font(.system(size: 19, weight: .bold))
            Text(s).font(.system(size: 10)).foregroundStyle(Self.muted)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 9).stroke(Self.rim))
    }

    private var griTile: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text("GLYCEMIA RISK INDEX").font(.system(size: 9, weight: .semibold)).foregroundStyle(Self.faint)
            HStack(spacing: 6) {
                Text(analytics.gri.value.map { String(format: "%.0f", $0) } ?? "--")
                    .font(.system(size: 19, weight: .bold))
                Text("Zone \(analytics.gri.zone ?? "—")")
                    .font(.system(size: 10, weight: .bold)).foregroundStyle(.white)
                    .padding(.horizontal, 7).padding(.vertical, 1)
                    .background(zoneColor(analytics.gri.zone), in: Capsule())
            }
            Text("hypo \(comp(analytics.gri.hypoComponent)) · hyper \(comp(analytics.gri.hyperComponent))")
                .font(.system(size: 10)).foregroundStyle(Self.muted)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 9).stroke(Self.rim))
    }

    // MARK: time in range goal bar

    private var tirGoalBar: some View {
        let rows: [(String, String, Color, Double, String?)] = [
            ("Very High", ">250", Self.bVHigh, analytics.veryHighPct, nil),
            ("High", "181–250", Self.bHigh, analytics.highPct, "High + Very High: <25%"),
            ("Target Range", "70–180", Self.bInRange, analytics.inRangePct, "Goal: >70% (>16h 48m)"),
            ("Low", "54–69", Self.bLow, analytics.lowPct, "Low + Very Low: <4%"),
            ("Very Low", "<54", Self.bVLow, analytics.veryLowPct, "Goal: <1%"),
        ]
        return VStack(alignment: .leading, spacing: 10) {
            sectionTitle("Time in Ranges")
            HStack(alignment: .top, spacing: 14) {
                // Vertical stacked bar, proportional to each band's share.
                VStack(spacing: 0) {
                    ForEach(rows.indices, id: \.self) { i in
                        Rectangle().fill(rows[i].2)
                            .frame(height: max(2, rows[i].3 / 100 * 210))
                    }
                }
                .frame(width: 60)
                .clipShape(RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(Self.rim))

                VStack(alignment: .leading, spacing: 7) {
                    ForEach(rows.indices, id: \.self) { i in
                        let r = rows[i]
                        HStack(spacing: 8) {
                            RoundedRectangle(cornerRadius: 3).fill(r.2).frame(width: 12, height: 12)
                            (Text(r.0).font(.system(size: 12, weight: .semibold))
                                + Text("  \(r.1)").font(.system(size: 11)).foregroundColor(Self.faint))
                            Spacer()
                            Text(String(format: "%.0f%%", r.3)).font(.system(size: 12, weight: .bold))
                        }
                        if let goal = r.4 {
                            Text(goal).font(.system(size: 10)).foregroundStyle(Self.faint)
                                .padding(.leading, 20).padding(.top, -3)
                        }
                    }
                }
            }
        }
    }

    // MARK: footer

    private var footer: some View {
        VStack(alignment: .leading, spacing: 4) {
            Rectangle().fill(Self.rim).frame(height: 1)
            (Text("Not a medical device. ").font(.system(size: 10, weight: .bold))
                + Text("Estimates for personal and clinical review, not a basis for treatment decisions. Bands and goals follow the 2019 international consensus (Battelino et al., Diabetes Care 2019); GMI = 3.31 + 0.02392 × mean; uGMI is the 2026 Diabetologia revision; GRI follows Klonoff et al. 2023.")
                .font(.system(size: 10)))
                .foregroundStyle(Self.muted)
            Text("NightKnight — private, self-hosted CGM").font(.system(size: 10)).foregroundStyle(Self.faint)
        }
    }

    private func sectionTitle(_ t: String) -> some View {
        Text(t.uppercased()).font(.system(size: 10, weight: .bold)).tracking(0.8).foregroundStyle(Self.muted)
    }

    // MARK: formatting

    private var rangeText: String {
        let f = DateFormatter(); f.dateFormat = "d MMM yyyy"
        return "\(f.string(from: Date(timeIntervalSince1970: Double(range.startMs) / 1000))) – \(f.string(from: Date(timeIntervalSince1970: Double(range.endMs) / 1000)))"
    }
    private var generatedText: String {
        let f = DateFormatter(); f.dateStyle = .medium; f.timeStyle = .short
        return f.string(from: Date(timeIntervalSince1970: Double(range.generatedMs) / 1000))
    }
    private func fmtGlu(_ mgdl: Double?) -> String {
        guard let m = mgdl else { return "--" }
        return GlucoseValue(mgdl: m).display(in: unit)
    }
    private func pct(_ v: Double?, _ digits: Int = 0) -> String { v == nil ? "--" : String(format: "%.\(digits)f", v!) }
    private func comp(_ v: Double?) -> String { v == nil ? "--" : String(format: "%.1f", v!) }
    private func zoneColor(_ zone: String?) -> Color {
        switch zone {
        case "A": return Self.bInRange
        case "B": return Color(red: 0.48, green: 0.72, blue: 0.25)
        case "C": return Color(red: 0.88, green: 0.72, blue: 0.24)
        case "D": return Self.bVHigh
        case "E": return Color(red: 0.78, green: 0.22, blue: 0.22)
        default: return Self.faint
        }
    }
}

/// Light-themed AGP percentile chart for the report (dark median line + green bands on
/// white), mirroring `AGPChartView`'s maths but tuned for paper.
private struct ReportAGPChart: View {
    let bins: [AgpBin]
    let unit: GlucoseUnit
    let lowMgdl: Double
    let highMgdl: Double

    private func conv(_ mgdl: Double) -> Double { unit == .mgdl ? mgdl : mgdl / GlucoseUnit.mgdlPerMmol }

    private func yDomain(_ pts: [AgpBin]) -> ClosedRange<Double> {
        let lo = pts.map { $0.p05 ?? $0.p50! }.min() ?? lowMgdl
        let hi = pts.map { $0.p95 ?? $0.p50! }.max() ?? highMgdl
        let yMin = max(40, floor((min(lo, lowMgdl) - 8) / 10) * 10)
        let yMax = ceil((max(hi, highMgdl) + 10) / 10) * 10
        return conv(yMin)...conv(yMax)
    }

    private let band = Color(red: 0.184, green: 0.62, blue: 0.34)

    var body: some View {
        let pts = bins.filter { $0.n > 0 && $0.p50 != nil }
        Group {
            if pts.count < 4 {
                Text("Not enough data for an AGP — needs about a day of readings.")
                    .font(.caption).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 200)
            } else {
                Chart {
                    ForEach(pts) { b in
                        AreaMark(x: .value("Time", b.minuteOfDay),
                                 yStart: .value("p05", conv(b.p05 ?? b.p50!)),
                                 yEnd: .value("p95", conv(b.p95 ?? b.p50!)),
                                 series: .value("Band", "5–95"))
                            .foregroundStyle(band.opacity(0.14)).interpolationMethod(.monotone)
                    }
                    ForEach(pts) { b in
                        AreaMark(x: .value("Time", b.minuteOfDay),
                                 yStart: .value("p25", conv(b.p25 ?? b.p50!)),
                                 yEnd: .value("p75", conv(b.p75 ?? b.p50!)),
                                 series: .value("Band", "25–75"))
                            .foregroundStyle(band.opacity(0.32)).interpolationMethod(.monotone)
                    }
                    RuleMark(y: .value("low", conv(lowMgdl)))
                        .lineStyle(StrokeStyle(lineWidth: 1, dash: [4, 4])).foregroundStyle(band.opacity(0.6))
                    RuleMark(y: .value("high", conv(highMgdl)))
                        .lineStyle(StrokeStyle(lineWidth: 1, dash: [4, 4])).foregroundStyle(band.opacity(0.6))
                    ForEach(pts) { b in
                        LineMark(x: .value("Time", b.minuteOfDay), y: .value("Median", conv(b.p50!)))
                            .foregroundStyle(Color(red: 0.075, green: 0.090, blue: 0.125))
                            .lineStyle(StrokeStyle(lineWidth: 2.4, lineCap: .round, lineJoin: .round))
                            .interpolationMethod(.monotone)
                    }
                }
                .chartXScale(domain: 0...1440)
                .chartXAxis {
                    AxisMarks(values: [0, 360, 720, 1080, 1440]) { value in
                        AxisGridLine()
                        AxisValueLabel {
                            if let m = value.as(Int.self) { Text(String(format: "%02d:00", m / 60)) }
                        }
                    }
                }
                .chartYScale(domain: yDomain(pts))
                .chartYAxis { AxisMarks(position: .leading) }
            }
        }
    }
}

// MARK: - PDF rendering + sharing

/// Rasterise a SwiftUI report view to a single-page PDF file, returning its URL.
enum ReportPDF {
    /// Render `content` (sized to A4 at 72 dpi, 595×842 pt) to a PDF in the temp directory.
    @MainActor
    static func render(_ content: some View, fileName: String) -> URL? {
        let size = CGSize(width: 595, height: 842)
        let renderer = ImageRenderer(content: content.frame(width: size.width, height: size.height))
        renderer.proposedSize = ProposedViewSize(size)
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(fileName)
        var produced = false
        renderer.render { _, drawInContext in
            var box = CGRect(origin: .zero, size: size)
            guard let consumer = CGDataConsumer(url: url as CFURL),
                  let ctx = CGContext(consumer: consumer, mediaBox: &box, nil) else { return }
            ctx.beginPDFPage(nil)
            drawInContext(ctx)
            ctx.endPDFPage()
            ctx.closePDF()
            produced = true
        }
        return produced ? url : nil
    }
}

/// A file to hand to the share sheet (identifiable so it can drive a `.sheet(item:)`).
struct ReportShareItem: Identifiable {
    let id = UUID()
    let url: URL
}

/// A thin wrapper around `UIActivityViewController` for AirDrop / Mail / Files / Print.
struct ReportShareSheet: UIViewControllerRepresentable {
    let items: [Any]
    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }
    func updateUIViewController(_ vc: UIActivityViewController, context: Context) {}
}

/// Write export text/data to a temp file and return its URL (for the share sheet).
enum ExportFile {
    static func write(_ text: String, name: String) -> URL? { write(Data(text.utf8), name: name) }
    static func write(_ data: Data, name: String) -> URL? {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(name)
        do { try data.write(to: url); return url } catch { return nil }
    }
}
