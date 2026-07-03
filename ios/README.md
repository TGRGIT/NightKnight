# NightKnight for iOS

A native SwiftUI client for a NightKnight deployment — current glucose + trend, a
Swift Charts trace, trailing A1c / average / time-in-range with a 1–90 day selector,
Apple Health read **and** write, on-device (disableable) alarms, and an
App-Intent–configured widget for the Home Screen, Lock Screen and StandBy.

> Phase 2 / in progress. The service (Cloudflare Worker / container) is the backend.

## Build

This repo keeps an [XcodeGen](https://github.com/yonaskolb/XcodeGen) spec instead of a
checked-in `.xcodeproj`:

```bash
brew install xcodegen
cd ios
xcodegen generate
open NightKnight.xcodeproj
```

Then in Xcode: set your **Development Team** (signing), and confirm the **App Group**
`group.be.cooney.nightknight` exists for both the app and widget targets (it shares
settings with the widget). HealthKit and Notifications capabilities are declared in
the entitlements / Info generated from `project.yml`.

### Rust analytics FFI (`ios/Rust`)

The direct data sources (see below) compute the full statistics **on-device** by
calling `nightknight-core`'s analytics through a small C ABI
(`service/crates/nightknight-ffi`), linked as the checked-in
`ios/Rust/NightKnightFFI.xcframework` — the app builds without a Rust toolchain.
After changing anything under `nightknight-ffi`/`nightknight-core`, rebuild and
recommit it (CI's staleness check compares the `NightKnightFFI.sha256` sidecar
against the sources and fails otherwise):

```bash
bash ios/scripts/build-rust-ffi.sh          # rebuild + refresh the sidecar
NK_REGEN_GOLDENS=1 cargo test -p nightknight-ffi --test golden   # after an intentional report change
```

The FFI's analytics/AGP JSON is byte-identical to the server's `/api/v4/*` payloads —
pinned by golden tests on both sides (`nightknight-ffi/tests/golden.rs` and
`NightKnightSourcesTests/RustAnalyticsTests.swift`, over shared `ios/Tests/Fixtures/`).

## Data sources (first-run chooser / Settings)

The app runs against one of four interchangeable sources — the three *direct* ones
need no NightKnight server at all:

- **NightKnight** (classic) — your deployment computes the analytics. Server URL +
  device token (+ optional Cloudflare Access service token). The only mode with
  reliable background alarms (silent push).
- **Dexcom Share** — Dexcom account username/password (unofficial follower API).
- **Libreview / LibreLinkUp** — LibreLinkUp email/password (unofficial, version-gated).
- **Nightscout** — your instance URL + api-secret; backfills the full history on
  first connect.

Direct sources accumulate raw readings in an App-Group SQLite store (`LocalStore`,
pruned to 90 days) and compute analytics via the Rust FFI; a Dexcom Clarity /
LibreView CSV import gives instant backfill. Exactly one source owns the local data
at a time — switching source or account wipes it (confirmed in the UI, enforced by a
DB owner-guard). Widgets/watch/complications never talk to a vendor: the app is the
sole fetcher and feeds them via `ReadingCache` (+ WatchConnectivity).

## Configure (in the app → Settings)

- **Data source** — one of the four above, with per-source credentials.
- **Unit** — mg/dL or mmol/L (both first-class).
- **Apple Health** — authorize, then toggle read/write.
- **Alarms** — master toggle + low/high thresholds + rapid-drop; all disableable.
  Best-effort in background for direct sources (no server push).

## Layout

```
ios/
  project.yml                 XcodeGen spec (app + widget targets)
  Shared/                     compiled into both targets
    GlucoseUnit.swift          units + 18.0156 conversion (mirrors the server)
    Models.swift               readings, trend, analytics, trailing periods
    Settings.swift             app-group settings; secrets in Keychain
    Keychain.swift
    APIClient.swift            /api/v4 client (current, entries, analytics)
  NightKnight/                 the app
    NightKnightApp.swift, DashboardView.swift, GlucoseChartView.swift,
    SettingsView.swift, Theme.swift, HealthKitManager.swift, AlarmManager.swift,
    Assets.xcassets/AppIcon…   (Hub System brand mark)
  NightKnightWidget/           App-Intent widget (WidgetKit) — iOS Home/Lock/StandBy
  NightKnightWatch/            watchOS app, embedded in the iOS app (current BG, trend, A1c/TIR)
    WatchApp / WatchDashboardView / WatchSyncManager (WatchConnectivity config)
  NightKnightWatchWidget/      Apple Watch complications (accessory families)
  NightKnightUITests/          XCUITests (period selector + Settings)
```

## The watch app (embedded — single binary)

The watch app is **embedded in the iOS app**, so one submission ships both:
`NightKnight.app/Watch/NightKnightWatch.app/PlugIns/NightKnightWatchWidget.appex`.
The embed uses `embed: true, link: false` in `project.yml` — `link: false` is
essential, otherwise Xcode links the watchOS app into the iOS binary and compiles the
watch widget for iOS.

Build the whole thing from the `NightKnight` scheme **without `-sdk`** so each target
uses its own SDK (passing `-sdk iphonesimulator` forces the watch targets to iOS):

```bash
xcodebuild -scheme NightKnight -destination 'generic/platform=iOS Simulator' \
  CODE_SIGNING_ALLOWED=NO build          # iOS app + embedded watch app + complication
```

Iterate on just the watch with the `NightKnightWatch` scheme
(`-sdk watchsimulator -destination 'generic/platform=watchOS Simulator'`).

The watch gets its server URL + token from the paired iPhone via WatchConnectivity
(`PhoneSyncManager` → `WatchSyncManager`). **Entitlements:** the watch app + watch
widget carry the App Group (`group.be.cooney.nightknight`); complications need *no*
dedicated entitlement (they're WidgetKit accessory widgets).

## Alarms

Alarms are **on/off only — there is no snooze** (by design). Toggle them from the bell
in the dashboard toolbar (instant silence) or in Settings, where you also set the
low/high thresholds and the rapid-drop alert. Nothing fires while off.

## Still to do

- Server-push alarms when the app is closed (Phase 3, APNs).
- Reading from Health as a data *source* (currently read-back only).
- Watch complication gallery screenshot (needs interactive watch-face editing).
