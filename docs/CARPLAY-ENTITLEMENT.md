# Requesting the CarPlay entitlement for NightKnight

CarPlay is **gated**: the templates are in the SDK, but your app can't present a
CarPlay UI until Apple grants your developer account a CarPlay **entitlement** for a
specific app category. You request it with a short form; Apple reviews the *concept*
(category fit + driver-distraction safety) before you write the integration.

This runbook covers both halves:

- **Part A — the request:** which entitlement to ask for, and a ready-to-paste packet.
- **Part B — after approval:** wiring the entitlement, provisioning, the CarPlay scene
  and a minimal glance UI into this repo (xcodegen-based).

> Apple changes the request form and the developer pages from time to time. Treat the
> *answers* here as canonical and re-confirm the *mechanics* against the live pages
> linked at the bottom before you submit.

---

## 0. TL;DR

| | |
|---|---|
| **Category to request** | **Driving Task** |
| **Entitlement key** | `com.apple.developer.carplay-driving-task` |
| **Where to request** | <https://developer.apple.com/contact/carplay/> |
| **What ships in CarPlay** | A glanceable current-glucose + trend screen (`CPInformationTemplate`), nothing that scrolls or distracts |
| **Templates allowed** | `CPInformationTemplate`, `CPListTemplate`, `CPGridTemplate`, `CPPointOfInterestTemplate`, `CPTabBarTemplate`, `CPAlert/ActionSheet` |
| **Not allowed** | The live 24h chart, AGP, scrubbing, anything requiring sustained attention |
| **Typical turnaround** | Days to a few weeks; not guaranteed |

---

## 1. Which entitlement — and why Driving Task

Apple only lets third parties build CarPlay apps in a fixed set of categories
(audio, communication, navigation, EV charging, fueling, parking, quick food ordering,
and **driving task**). Each needs its own entitlement, and you must pick the one that
genuinely describes your app.

**NightKnight is a Driving Task app.** The Driving Task category exists for apps that
let someone *"accomplish an important task without taking out their phone"* — short,
glanceable interactions, not media or navigation. A person with diabetes checking
their current glucose and trend at a glance is exactly that: today they'd pick up the
phone (unsafe); CarPlay lets them glance at the car's built-in screen instead.

Why not the others:

- **Navigation / Audio / EV / Fueling / Parking / Quick-ordering** describe unrelated
  jobs and unlock template sets (maps, now-playing, POI ordering) NightKnight doesn't use.
- There is **no general "show me a number" entitlement** — Driving Task is the
  closest legitimate fit and the one Apple steers glanceable utilities toward.

Keep the CarPlay surface deliberately minimal. Apple's #1 concern is driver
distraction; a single glanceable screen is both the safest design *and* the easiest to
get approved.

---

## 2. Prerequisites

- Membership in the Apple Developer Program (the account that owns the App ID).
- The App ID / bundle id finalised: `be.cooney.nightknight.NightKnight`.
- Ideally the app already on TestFlight or the App Store — a shipping, populated app
  makes the safety story concrete and approval faster (not strictly required).
- A one-screen **mockup** of the CarPlay UI (see §3.3) to attach to the request.

---

## Part A — The request

### 3.1 Submit the form

1. Sign in and open **<https://developer.apple.com/contact/carplay/>**
   (also reachable from <https://developer.apple.com/carplay/> → *Request CarPlay App
   Entitlement*).
2. Choose app type **Driving Task**.
3. Fill in the fields using the packet in §3.2.
4. Attach the mockup(s) from §3.3.
5. Submit, and watch the email tied to your developer account for the grant (the
   entitlement then appears against your account in the Developer portal).

### 3.2 Ready-to-paste answers

> Edit the bracketed bits. Keep the safety language — it's what's actually being
> reviewed.

**App name:** NightKnight

**Bundle ID:** be.cooney.nightknight.NightKnight

**App Store URL:** [link, or "In TestFlight / pre-release"]

**CarPlay app category:** Driving Task

**What does your app do?**
> NightKnight is a private follower/viewer for continuous glucose monitoring (CGM) data.
> It connects to the user's own NightKnight or Nightscout-compatible server and shows
> their current glucose value, trend direction and how long ago it was read, plus
> optional high/low alerts. It is not a medical device; it presents information the user
> already owns.

**What will your app do in CarPlay, specifically?**
> A single, glanceable screen showing the latest glucose value, a colour/word status
> (e.g. "In range"), the trend arrow, and the reading's age. It refreshes automatically
> in the background on the CGM cadence (about every 5 minutes). Optionally, a low/high
> reading raises a standard CarPlay alert with audio so the driver is told to check
> their phone or pull over — without reading anything detailed while moving.

**Why does it belong in CarPlay / why a Driving Task app?**
> Checking current glucose is a short, important task a driver would otherwise do by
> picking up and unlocking their phone — a known distraction. Surfacing only the current
> value and trend on the car's display lets them glance once, the same way they glance at
> fuel or speed, and keep their attention on the road.

**How have you minimised driver distraction?**
> • One screen, no scrolling, no charts, no interaction required — it just displays.
> • Large, high-contrast value using CarPlay's own templates and system fonts.
> • No history, no graphs, no data entry, no settings in the car (all configured on the
>   phone beforehand).
> • Updates happen automatically; the driver never taps to refresh.
> • Out-of-range states use a brief standard alert with audio rather than detail to read.
> • Fully compliant with Apple's CarPlay App Design and Human Interface Guidelines for
>   Driving Task apps.

**Who is the audience / how widely used?** [e.g. "People with diabetes who self-host
their CGM data; [N] TestFlight users."]

### 3.3 The mockup to attach

The form wants to see the proposed CarPlay UI. You don't need a working build yet — a
static mockup is fine. Make **one** 800×480-ish landscape frame showing:

- A `CPInformationTemplate`-style layout: title "Glucose", then 2–3 large rows:
  - **`124 mg/dL`** (colour by range)
  - **`→ Steady`**
  - **`Updated 1 min ago`**
- Dark theme, matching the app.

Reuse the look from the existing iOS glance UI (the dashboard current-card in
`ios/NightKnight/DashboardView.swift`). A quick way to produce the frame: open the
**CarPlay Simulator** (Part B, §7) against a throwaway `CPInformationTemplate`, or mock
it in any design tool. Keep it obviously glanceable.

---

## Part B — After Apple grants it

Once the entitlement is on your account, regenerate provisioning and wire it up.

### 4. Provisioning

1. In the Developer portal, the App ID **be.cooney.nightknight.NightKnight** now offers
   the **CarPlay (Driving Task)** capability — enable it.
2. Regenerate the **development** and **distribution** provisioning profiles so they
   include the entitlement (Xcode "Automatically manage signing" will do this once the
   capability is on the App ID and the entitlement is in the project — see §5).

### 5. Add the entitlement (xcodegen)

This project is generated from `ios/project.yml`, so add the entitlement there (not by
hand in Xcode, which would be overwritten on the next `xcodegen generate`).

In `ios/project.yml`, under `targets: → NightKnight: → entitlements: → properties:`,
add:

```yaml
        com.apple.developer.carplay-driving-task: true
```

So the block becomes:

```yaml
    entitlements:
      path: NightKnight/NightKnight.entitlements
      properties:
        com.apple.developer.healthkit: true
        com.apple.developer.healthkit.access: [health-records]
        com.apple.security.application-groups: [group.be.cooney.nightknight]
        aps-environment: development
        com.apple.developer.carplay-driving-task: true   # ← add
```

Then `cd ios && xcodegen generate`.

### 6. Declare the CarPlay scene + delegate

CarPlay is a second **scene** on top of your normal window. Add a scene manifest and a
delegate to the **NightKnight** target's `Info` properties in `project.yml`:

```yaml
    info:
      path: NightKnight/Info.plist
      properties:
        # ...existing keys...
        UIApplicationSceneManifest:
          UIApplicationSupportsMultipleScenes: true
          UISceneConfigurations:
            CPTemplateApplicationSceneSessionRoleApplication:
              - UISceneConfigurationName: NightKnight-CarPlay
                UISceneDelegateClassName: $(PRODUCT_MODULE_NAME).CarPlaySceneDelegate
```

> If you also want to keep an explicit phone-scene delegate you can add a
> `UIWindowSceneSessionRoleApplication` entry, but with SwiftUI's `App`/`WindowGroup`
> you can leave the phone scene implicit and only declare the CarPlay one.

### 7. Minimal glance UI

Add `ios/NightKnight/CarPlaySceneDelegate.swift`. It reuses the existing `APIClient`,
`Settings`, `ReadingCache`, `GlucoseValue`/`GlucoseBand` and `CurrentReading` — no new
data layer. A `CPInformationTemplate` is the right glanceable surface.

```swift
import CarPlay

final class CarPlaySceneDelegate: UIResponder, CPTemplateApplicationSceneDelegate {
    private var interfaceController: CPInterfaceController?
    private var refreshTask: Task<Void, Never>?

    func templateApplicationScene(_ scene: CPTemplateApplicationScene,
                                  didConnect interfaceController: CPInterfaceController) {
        self.interfaceController = interfaceController
        let template = CPInformationTemplate(title: "Glucose", layout: .leading,
                                             items: [], actions: [])
        interfaceController.setRootTemplate(template, animated: false, completion: nil)
        startRefreshing(template)
    }

    func templateApplicationScene(_ scene: CPTemplateApplicationScene,
                                  didDisconnect interfaceController: CPInterfaceController) {
        refreshTask?.cancel()
        self.interfaceController = nil
    }

    /// Glanceable only: current value, status, trend, age. Refreshes on CGM cadence.
    private func startRefreshing(_ template: CPInformationTemplate) {
        refreshTask?.cancel()
        refreshTask = Task { @MainActor in
            while !Task.isCancelled {
                let reading = (try? await APIClient(settings: .shared).current())
                    ?? ReadingCache.load()            // warm fallback the app/widget keep fresh
                template.items = items(for: reading)
                try? await Task.sleep(for: .seconds(60))
            }
        }
    }

    private func items(for reading: CurrentReading?) -> [CPInformationItem] {
        guard let r = reading else {
            return [CPInformationItem(title: "Glucose", detail: "Open NightKnight on your phone")]
        }
        let unit = Settings.shared.preferredUnit
        let band = GlucoseBand.of(mgdl: r.value.mgdl)
        let age = Int(Date().timeIntervalSince(r.date) / 60)
        return [
            CPInformationItem(title: "\(r.value.display(in: unit)) \(unit.label)", detail: band.label),
            CPInformationItem(title: "Trend", detail: "\(r.trend.glyph)  \(r.trendLabel)"),
            CPInformationItem(title: "Updated", detail: age <= 0 ? "just now" : "\(age) min ago"),
        ]
    }
}
```

Notes:
- **No charts in the car.** Keep it to `CPInformationItem` rows. The 24h chart, AGP and
  episodes stay phone-only by design (and by Apple policy).
- Background freshness already exists: the widget/watch keep `ReadingCache` warm and the
  app schedules background refreshes (`BackgroundRefresh` in `NightKnightApp.swift`), so
  the CarPlay glance has a recent value even before its own fetch returns.
- For out-of-range alerting in the car, present a brief `CPAlertTemplate` from your
  existing `AlarmManager` evaluation rather than anything the driver must read.

### 8. Test in the CarPlay Simulator

No CarPlay hardware needed:

1. Run the app on an iOS **Simulator**.
2. **Xcode → Open Developer Tool → Simulator**, then in the Simulator menu
   **I/O → External Displays → CarPlay**.
3. The CarPlay screen appears; your `CPInformationTemplate` should render with live
   (or demo) data. The `-NKDemo` launch argument (see `ios/Shared/DemoData.swift`)
   populates it without a server.

### 9. App Review notes (CarPlay)

When you submit the build that adds CarPlay, add to the review notes:

> This version adds a CarPlay Driving Task screen: a single glanceable view of the
> user's current glucose value, status, trend and reading age — no charts, no scrolling,
> no interaction while driving. CarPlay can be exercised in the iOS Simulator via
> I/O → External Displays → CarPlay. The entitlement
> `com.apple.developer.carplay-driving-task` was granted on [date].

---

## Checklist

- [ ] Request submitted at developer.apple.com/contact/carplay (category: Driving Task)
- [ ] Mockup attached (glanceable value + trend + age)
- [ ] Entitlement granted on the developer account
- [ ] CarPlay capability enabled on the App ID; profiles regenerated
- [ ] `com.apple.developer.carplay-driving-task: true` added in `project.yml`
- [ ] `UIApplicationSceneManifest` + `CarPlaySceneDelegate` added; `xcodegen generate`
- [ ] `CarPlaySceneDelegate.swift` renders a `CPInformationTemplate` glance
- [ ] Verified in the CarPlay Simulator (with `-NKDemo`)
- [ ] App Review notes mention the CarPlay screen + grant date

---

## References

- CarPlay overview & request — <https://developer.apple.com/carplay/>
- Request CarPlay entitlement — <https://developer.apple.com/contact/carplay/>
- Requesting CarPlay Entitlements (docs) — <https://developer.apple.com/documentation/carplay/requesting-carplay-entitlements>
- `CPTemplateApplicationSceneDelegate` — <https://developer.apple.com/documentation/carplay/cptemplateapplicationscenedelegate>
- `CPInformationTemplate` — <https://developer.apple.com/documentation/carplay/cpinformationtemplate>
- CarPlay App Design / HIG — <https://developer.apple.com/design/human-interface-guidelines/carplay>
- CarPlay Developer Guide (PDF) — <https://developer.apple.com/download/files/CarPlay-Developer-Guide.pdf>
