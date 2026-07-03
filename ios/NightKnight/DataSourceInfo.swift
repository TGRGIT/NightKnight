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

    /// Chooser order: the two sign-in-and-go options first, then the two that need
    /// your own server. Copy is plain-language and mechanism-free (no push/APNs) so
    /// the chooser is easy to skim; the "?" sheet carries the trade-offs.
    static let all: [DataSourceInfo] = [
        DataSourceInfo(
            source: .dexcom,
            tagline: "Sign in with your Dexcom account. Easiest to set up — nothing to run yourself.",
            pros: [
                "Just your Dexcom account — no server to set up",
                "New readings arrive every few minutes",
                "Import up to 90 days of history from a Dexcom Clarity export during setup",
            ],
            cons: [
                "Uses Dexcom's unofficial follower service, which Dexcom can change or break",
                "Live readings only cover the last day; older history comes from the CSV import",
            ]),
        DataSourceInfo(
            source: .libre,
            tagline: "Sign in with your LibreLinkUp account. Easiest to set up — nothing to run yourself.",
            pros: [
                "Just your LibreLinkUp account — no server to set up",
                "Import up to 90 days of history from a LibreView export during setup",
            ],
            cons: [
                "Uses Abbott's unofficial follower service, which Abbott can change or break",
                "Live readings only cover about the last 12 hours; older history comes from the CSV import",
            ]),
        DataSourceInfo(
            source: .nightknight,
            tagline: "Connect to your own NightKnight server. The most private and self-contained option.",
            pros: [
                "Your data stays on your own infrastructure",
                "Protected by Cloudflare Access and encrypted at rest",
                "Your server does the analytics, so the app stays light",
            ],
            cons: [
                "You have to deploy and run the NightKnight server yourself",
            ]),
        DataSourceInfo(
            source: .nightscout,
            tagline: "Connect to a Nightscout site you already run.",
            pros: [
                "Works with the wider open-source Nightscout ecosystem",
                "Your data stays on your own instance",
                "Imports your full history automatically the first time it connects",
            ],
            cons: [
                "You have to run a Nightscout site yourself",
                "How secure it is depends on how you set that site up",
            ]),
    ]

    static func info(for source: DataSource) -> DataSourceInfo {
        // `all` covers every case; a miss is a programmer error caught in DEBUG.
        all.first { $0.source == source } ?? all[0]
    }
}
