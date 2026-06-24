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
        .frame(height: 240)
        .overlay {
            if readings.isEmpty {
                Text("No glucose data yet.").foregroundStyle(.secondary)
            }
        }
    }
}
