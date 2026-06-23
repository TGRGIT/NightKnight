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

## Configure (in the app → Settings)

- **Server URL** — your deployment, e.g. `https://nightknight.cooney.be`.
- **Device token** — create one in the web UI (*Devices & tokens*); sent as the
  `api-secret` header.
- **Cloudflare Access** (optional) — a service token (`CF-Access-Client-Id` /
  `-Secret`) so the app can pass the Access edge gate when deployed behind it.
- **Unit** — mg/dL or mmol/L (both first-class).
- **Apple Health** — authorize, then toggle read/write.
- **Alarms** — master toggle + low/high thresholds + rapid-drop; all disableable.

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
