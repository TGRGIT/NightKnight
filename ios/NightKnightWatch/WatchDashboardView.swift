import SwiftUI

@MainActor
@Observable
final class WatchModel {
    var current: CurrentReading?
    var analytics: GlucoseAnalytics?
    var errorText: String?
    private var client: APIClient { APIClient(settings: .shared) }

    func refresh() async {
        // Local-analytics sources: the watch never holds vendor credentials and never
        // logs in — the phone pushes each new reading over WatchConnectivity into this
        // watch's ReadingCache. (Richer analytics tiles on the watch in a local source
        // are a documented follow-up; the summary line just stays empty.)
        if Settings.shared.usesLocalAnalytics {
            current = ReadingCache.load()
            analytics = nil
            errorText = current == nil ? "Open NightKnight on your iPhone to sync readings" : nil
            return
        }
        do {
            async let c = client.current()
            async let a = client.analytics(hours: 24)
            current = try await c
            analytics = try await a
            errorText = nil
            if let current { ReadingCache.save(current) }   // keep the complication's fallback warm
        } catch {
            errorText = (error as? APIError)?.errorDescription ?? "—"
        }
    }
}

private func bandColor(_ mgdl: Double) -> Color {
    switch GlucoseBand.of(mgdl: mgdl) {
    case .veryLow, .veryHigh: return .red
    case .low, .high: return .orange
    case .inRange: return .green
    }
}

/// The watch app's main view: current glucose + trend, with a compact trailing line.
struct WatchDashboardView: View {
    @State private var model = WatchModel()
    private var unit: GlucoseUnit { Settings.shared.preferredUnit }

    var body: some View {
        VStack(spacing: 3) {
            if let c = model.current {
                Text(c.value.display(in: unit))
                    .font(.system(size: 46, weight: .bold, design: .rounded))
                    .foregroundStyle(bandColor(c.value.mgdl))
                Text("\(unit.label)  \(c.trend.glyph)")
                    .font(.footnote).foregroundStyle(.secondary)
                Text(c.date, style: .relative).font(.caption2).foregroundStyle(.secondary)
                // Lead with uGMI (the preferred A1c estimate); name it explicitly rather
                // than a bare "A1c", and fall back to eA1c only if an old server omits it.
                if let a = model.analytics, let a1c = a.uGmiPercent ?? a.estimatedA1cPercent {
                    let label = a.uGmiPercent != nil ? "uGMI" : "eA1c"
                    Text(String(format: "%@ %.1f%% · TIR %.0f%%", label, a1c, a.inRangePct))
                        .font(.caption2).foregroundStyle(.secondary).padding(.top, 2)
                }
            } else if let err = model.errorText {
                Text(err).font(.caption2).foregroundStyle(.secondary).multilineTextAlignment(.center)
            } else {
                ProgressView()
            }
        }
        .containerBackground(.black.gradient, for: .navigation)
        .task {
            while !Task.isCancelled {
                await model.refresh()
                try? await Task.sleep(for: .seconds(60))
            }
        }
    }
}
