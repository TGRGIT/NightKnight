import SwiftUI

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
            if let current {
                ReadingCache.save(current)   // keep the widget's fallback warm
                AlarmManager.shared.evaluate(current, settings: settings)
            }
            if settings.writeToHealthKit { await HealthKitManager.shared.write(readings) }
        } catch {
            errorText = (error as? APIError)?.errorDescription ?? error.localizedDescription
        }
    }

    func reloadAnalytics() async {
        analytics = try? await client.analytics(hours: period.hours)
    }
}

struct DashboardView: View {
    @State private var model = DashboardModel()
    @State private var showSettings = false
    private var unit: GlucoseUnit { model.settings.preferredUnit }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 16) {
                    currentCard
                    summaryCard
                    chartCard
                }
                .padding()
            }
            .background(Color.nkInk.ignoresSafeArea())
            .navigationTitle("NightKnight")
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    // Quick alarm on/off. Tapping off silences all alarms immediately;
                    // turning on requests notification permission. (No snooze — alarms
                    // are simply on or off.)
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
                ToolbarItem(placement: .topBarTrailing) {
                    Button { showSettings = true } label: { Image(systemName: "gearshape") }
                        .accessibilityIdentifier("settingsButton")
                        .accessibilityLabel("Settings")
                }
            }
            .sheet(isPresented: $showSettings) { SettingsView() }
            .task { await pollLoop() }
            .refreshable { await model.refresh() }
        }
        .tint(Color.nkAccent)
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
        return VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 14) {
                Text(model.current?.value.display(in: unit) ?? "--")
                    .font(.system(size: 68, weight: .bold, design: .rounded))
                    .foregroundStyle(band?.color ?? .primary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(unit.label).foregroundStyle(.secondary)
                    HStack(spacing: 6) {
                        Text(model.current?.trend.glyph ?? "·").font(.title)
                        if let t = model.current?.trend.label, !t.isEmpty {
                            Text(t).font(.subheadline).foregroundStyle(.secondary)
                        }
                    }
                }
                Spacer()
            }
            if let c = model.current, let band {
                // Level (Urgent low … Urgent high) and trend are shown together — the
                // two distinct things a person checks at a glance.
                HStack(spacing: 8) {
                    Text(band.label).font(.caption.weight(.semibold)).foregroundStyle(band.color)
                    Text(c.date, style: .relative).font(.caption).foregroundStyle(.secondary)
                }
            } else if let err = model.errorText {
                Text(err).font(.caption).foregroundStyle(Color.nkAccent)
            }
        }
        .padding(20).frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.nkTile, in: RoundedRectangle(cornerRadius: 20))
    }

    private var summaryCard: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text("TRAILING SUMMARY").font(.caption).bold().foregroundStyle(.secondary)
                Spacer()
            }
            Picker("Period", selection: $model.period) {
                ForEach(TrailingPeriod.allCases) { Text($0.label).tag($0) }
            }
            .pickerStyle(.segmented)
            .onChange(of: model.period) { Task { await model.reloadAnalytics() } }

            let a = model.analytics
            HStack(spacing: 12) {
                metric("Est. A1c", a?.estimatedA1cPercent.map { String(format: "%.1f%%", $0) } ?? "--",
                       sub: a?.gmiPercent.map { String(format: "GMI %.1f%%", $0) } ?? " ")
                metric("Avg", a?.meanMgdl.map { GlucoseValue(mgdl: $0).display(in: unit) } ?? "--",
                       sub: unit.label)
            }
            HStack(spacing: 12) {
                metric("In range", a.map { String(format: "%.0f%%", $0.inRangePct) } ?? "--",
                       sub: a.map { "\($0.n) readings" } ?? " ")
                metric("CV", a?.cvPercent.map { String(format: "%.0f%%", $0) } ?? "--",
                       sub: a?.cvPercent.map { $0 <= 36 ? "stable" : "variable" } ?? " ")
            }
            if let a { TIRBar(analytics: a) }
        }
        .padding(20)
        .background(Color.nkTile, in: RoundedRectangle(cornerRadius: 20))
    }

    private var chartCard: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("GLUCOSE").font(.caption).bold().foregroundStyle(.secondary)
            GlucoseChartView(readings: model.readings, unit: unit,
                             lowMgdl: model.settings.lowThresholdMgdl,
                             highMgdl: model.settings.highThresholdMgdl)
        }
        .padding(20)
        .background(Color.nkTile, in: RoundedRectangle(cornerRadius: 20))
    }

    private func metric(_ label: String, _ value: String, sub: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label.uppercased()).font(.caption2).foregroundStyle(.secondary)
            Text(value).font(.system(.title, design: .rounded)).bold()
            Text(sub).font(.caption2).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(Color.nkInk, in: RoundedRectangle(cornerRadius: 12))
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
