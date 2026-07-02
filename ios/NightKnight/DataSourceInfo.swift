import Foundation

/// The chooser/settings copy for one data source — single-sourced so the onboarding
/// cards, the "?" pros/cons popovers, and SettingsView all show the same text.
struct DataSourceInfo: Identifiable {
    let source: DataSource
    let tagline: String
    let pros: [String]
    let cons: [String]

    var id: DataSource { source }
    var title: String { source.label }

    /// Chooser order: the two zero-infrastructure options first, then the two
    /// self-hosted ones.
    static let all: [DataSourceInfo] = [
        DataSourceInfo(
            source: .dexcom,
            tagline: "Sign in with your Dexcom account — nothing to host.",
            pros: [
                "No server to run",
                "Easy setup — just your Dexcom Share login",
                "Near-real-time readings (~5 min)",
            ],
            cons: [
                "Unofficial follower API — Dexcom can break it at any time",
                "Starts with only recent history; builds up over time (or import a Clarity CSV)",
                "Background alarms are best-effort — no server push while the app is closed",
            ]),
        DataSourceInfo(
            source: .libre,
            tagline: "Sign in with LibreLinkUp — nothing to host.",
            pros: [
                "No server to run",
                "Easy setup — just your LibreLinkUp login",
            ],
            cons: [
                "Unofficial, version-gated API — Abbott can break it at any time",
                "~12-hour rolling window; history builds up over time (or import a LibreView CSV)",
                "Background alarms are best-effort — no server push while the app is closed",
            ]),
        DataSourceInfo(
            source: .nightknight,
            tagline: "Your own NightKnight server does the heavy lifting.",
            pros: [
                "Strong security — Cloudflare Access gate, encrypted at rest",
                "Data independence — your data on your infrastructure",
                "Server-side analytics and reliable background alarms via silent push",
            ],
            cons: [
                "You deploy and run the NightKnight service",
            ]),
        DataSourceInfo(
            source: .nightscout,
            tagline: "Point at your existing Nightscout instance.",
            pros: [
                "Best compatibility with the wider CGM ecosystem",
                "Open-source community",
                "Data independence",
                "Backfills your full history on first connect",
            ],
            cons: [
                "You run a Nightscout instance",
                "Security depends on how your instance is set up",
                "Background alarms are best-effort unless your setup pushes",
            ]),
    ]

    static func info(for source: DataSource) -> DataSourceInfo {
        // `all` covers every case; a miss is a programmer error caught in DEBUG.
        all.first { $0.source == source } ?? all[0]
    }
}
