import Charts
import SwiftUI

/// The glucose trace: a shaded target band, hi/lo threshold lines, and points coloured by
/// glucose band over a fixed, always-24-hour window with a TIME (not date) x-axis. Drag
/// across it to scrub — a marker and a value+time callout follow your finger.
struct GlucoseChartView: View {
    let readings: [GlucoseReading]
    let unit: GlucoseUnit
    let lowMgdl: Double
    let highMgdl: Double
    @State private var selected: GlucoseReading?

    private func conv(_ mgdl: Double) -> Double {
        unit == .mgdl ? mgdl : mgdl / GlucoseUnit.mgdlPerMmol
    }

    private var yDomain: ClosedRange<Double> {
        let maxReading = max(260, readings.map(\.mgdl).max() ?? 260)
        return conv(40)...conv(min(maxReading + 20, 600))
    }

    var body: some View {
        // A consistent 24-hour window ending now, so the axis means the same thing every
        // time regardless of how much data is present.
        let end = Date()
        let start = end.addingTimeInterval(-24 * 3600)
        Chart {
            RectangleMark(
                yStart: .value("low", conv(lowMgdl)),
                yEnd: .value("high", conv(highMgdl))
            )
            .foregroundStyle(Color.nkInRange.opacity(0.12))

            ForEach([lowMgdl, highMgdl], id: \.self) { t in
                RuleMark(y: .value("threshold", conv(t)))
                    .lineStyle(StrokeStyle(lineWidth: 1, dash: [4, 4]))
                    .foregroundStyle(Color.nkMuted.opacity(0.35))
            }

            ForEach(readings) { r in
                PointMark(
                    x: .value("Time", r.date),
                    y: .value("Glucose", r.value.value(in: unit))
                )
                .symbolSize(14)
                .foregroundStyle(GlucoseBand.of(mgdl: r.mgdl).color)
            }

            if let sel = selected {
                RuleMark(x: .value("Time", sel.date))
                    .lineStyle(StrokeStyle(lineWidth: 1))
                    .foregroundStyle(Color.nkMuted.opacity(0.55))
                    .annotation(position: .top, spacing: 0,
                                overflowResolution: .init(x: .fit(to: .chart), y: .disabled)) {
                        scrubLabel(sel)
                    }
                PointMark(
                    x: .value("Time", sel.date),
                    y: .value("Glucose", sel.value.value(in: unit))
                )
                .symbolSize(90)
                .foregroundStyle(GlucoseBand.of(mgdl: sel.mgdl).color)
            }
        }
        .chartXScale(domain: start...end)
        .chartYScale(domain: yDomain)
        .chartYAxis { AxisMarks(position: .leading) }
        .chartXAxis {
            // Times across the day (every 6 hours), never dates.
            AxisMarks(values: .stride(by: .hour, count: 6)) { _ in
                AxisGridLine()
                AxisTick()
                AxisValueLabel(format: .dateTime.hour())
            }
        }
        .frame(minHeight: 120, maxHeight: .infinity)
        .chartOverlay { proxy in
            GeometryReader { geo in
                Rectangle().fill(.clear).contentShape(Rectangle())
                    .gesture(
                        DragGesture(minimumDistance: 0)
                            .onChanged { value in
                                guard let plotFrame = proxy.plotFrame else { return }
                                let x = value.location.x - geo[plotFrame].origin.x
                                if let date: Date = proxy.value(atX: x) {
                                    selected = nearest(to: date)
                                }
                            }
                            .onEnded { _ in selected = nil }
                    )
            }
        }
        .overlay {
            if readings.isEmpty {
                Text("No glucose data yet.").foregroundStyle(.secondary)
            }
        }
    }

    /// The reading closest in time to a scrub position.
    private func nearest(to date: Date) -> GlucoseReading? {
        readings.min(by: {
            abs($0.date.timeIntervalSince(date)) < abs($1.date.timeIntervalSince(date))
        })
    }

    /// The value + time callout shown above the scrub line.
    @ViewBuilder private func scrubLabel(_ r: GlucoseReading) -> some View {
        VStack(spacing: 1) {
            HStack(spacing: 3) {
                Text(r.value.display(in: unit))
                    .font(.caption).bold()
                    .foregroundStyle(GlucoseBand.of(mgdl: r.mgdl).color)
                Text(unit.label).font(.caption2).foregroundStyle(.secondary)
            }
            Text(r.date, format: .dateTime.hour().minute())
                .font(.caption2).foregroundStyle(.secondary)
        }
        .padding(.horizontal, 8).padding(.vertical, 4)
        .background(Color.nkTile, in: RoundedRectangle(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.nkMuted.opacity(0.3)))
        .fixedSize()
    }
}

/// The Ambulatory Glucose Profile: every day overlaid onto one 24-hour axis as
/// percentile bands (5–95 outer, 25–75 IQR) around a median line, mirroring the web.
struct AGPChartView: View {
    let bins: [AgpBin]
    let unit: GlucoseUnit
    let lowMgdl: Double
    let highMgdl: Double

    private func conv(_ mgdl: Double) -> Double {
        unit == .mgdl ? mgdl : mgdl / GlucoseUnit.mgdlPerMmol
    }

    var body: some View {
        let pts = bins.filter { $0.n > 0 && $0.p50 != nil }
        Group {
            if pts.count < 4 {
                Text("Not enough data yet for an AGP — needs about a day of readings.")
                    .font(.caption).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 200)
            } else {
                Chart {
                    ForEach(pts) { b in
                        AreaMark(
                            x: .value("Time", b.minuteOfDay),
                            yStart: .value("p05", conv(b.p05 ?? b.p50!)),
                            yEnd: .value("p95", conv(b.p95 ?? b.p50!))
                        )
                        .foregroundStyle(Color.nkInRange.opacity(0.13))
                        // Monotone (shape-preserving), NOT catmullRom: the percentile
                        // edges (esp. p95/p05) still vary bin-to-bin even after the
                        // server's smoothing, and catmullRom overshoots those into
                        // spikes. Monotone never exceeds the data points — clean clinical
                        // envelopes, matching the web's straight-segment AGP bands.
                        .interpolationMethod(.monotone)
                    }
                    ForEach(pts) { b in
                        AreaMark(
                            x: .value("Time", b.minuteOfDay),
                            yStart: .value("p25", conv(b.p25 ?? b.p50!)),
                            yEnd: .value("p75", conv(b.p75 ?? b.p50!))
                        )
                        .foregroundStyle(Color.nkInRange.opacity(0.28))
                        .interpolationMethod(.monotone)
                    }
                    RuleMark(y: .value("low", conv(lowMgdl)))
                        .lineStyle(StrokeStyle(lineWidth: 1, dash: [4, 4]))
                        .foregroundStyle(Color.nkInRange.opacity(0.5))
                    RuleMark(y: .value("high", conv(highMgdl)))
                        .lineStyle(StrokeStyle(lineWidth: 1, dash: [4, 4]))
                        .foregroundStyle(Color.nkInRange.opacity(0.5))
                    ForEach(pts) { b in
                        // Explicit off-white (not `.primary`, which the chart's tint can
                        // resolve to red) — the median should read neutral over the band.
                        LineMark(x: .value("Time", b.minuteOfDay), y: .value("Median", conv(b.p50!)))
                            .foregroundStyle(Color(white: 0.95))
                            .lineStyle(StrokeStyle(lineWidth: 2.2, lineCap: .round, lineJoin: .round))
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
                .chartYAxis { AxisMarks(position: .leading) }
                .frame(height: 220)
            }
        }
    }
}
