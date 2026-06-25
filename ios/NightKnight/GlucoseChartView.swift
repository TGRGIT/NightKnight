import Charts
import SwiftUI

/// The glucose trace, mirroring the web chart: a shaded target band, hi/lo threshold
/// lines, and points coloured by glucose band. Values display in the chosen unit.
struct GlucoseChartView: View {
    let readings: [GlucoseReading]
    let unit: GlucoseUnit
    let lowMgdl: Double
    let highMgdl: Double

    private func conv(_ mgdl: Double) -> Double {
        unit == .mgdl ? mgdl : mgdl / GlucoseUnit.mgdlPerMmol
    }

    private var yDomain: ClosedRange<Double> {
        let maxReading = max(260, readings.map(\.mgdl).max() ?? 260)
        return conv(40)...conv(min(maxReading + 20, 600))
    }

    var body: some View {
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
                .symbolSize(16)
                .foregroundStyle(GlucoseBand.of(mgdl: r.mgdl).color)
            }
        }
        .chartYScale(domain: yDomain)
        .chartYAxis { AxisMarks(position: .leading) }
        .chartXAxis { AxisMarks(values: .automatic(desiredCount: 5)) }
        .frame(minHeight: 120, maxHeight: .infinity)
        .overlay {
            if readings.isEmpty {
                Text("No glucose data yet.").foregroundStyle(.secondary)
            }
        }
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
                        .interpolationMethod(.catmullRom)
                    }
                    ForEach(pts) { b in
                        AreaMark(
                            x: .value("Time", b.minuteOfDay),
                            yStart: .value("p25", conv(b.p25 ?? b.p50!)),
                            yEnd: .value("p75", conv(b.p75 ?? b.p50!))
                        )
                        .foregroundStyle(Color.nkInRange.opacity(0.28))
                        .interpolationMethod(.catmullRom)
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
                            .interpolationMethod(.catmullRom)
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
