# NightKnight — App Store listing copy

All fields below respect App Store Connect limits (character counts shown in
`[brackets]`). Pick one option where alternatives are given. Wording is kept
**non-diagnostic** on purpose — NightKnight displays and analyses glucose data you
already own; it is not a medical device and makes no treatment claims (App Review
Guideline 1.4.1 / 5.1.1).

---

## App Name  `[≤30]`

**Primary:** `NightKnight: Glucose Viewer`  `[27]`

Alternatives:
- `NightKnight — CGM Dashboard`  `[27]`
- `NightKnight: Glucose & CGM`  `[26]`
- `NightKnight`  `[11]`  (clean, brand-only)

## Subtitle  `[≤30]`

**Primary:** `Private CGM glucose dashboard`  `[29]`

Alternatives:
- `Your glucose, private & clear`  `[29]`
- `Secure Nightscout companion`  `[27]`
- `Time in range, AGP & alarms`  `[27]`

## Promotional Text  `[≤170]`  (editable any time, no review)

> Your glucose, beautifully clear and completely private. NightKnight turns your own
> CGM server into a live dashboard with deep analytics — no ads, no data resale.  `[161]`

---

## Keywords  `[≤100]`  (comma-separated, no spaces between items)

```
glucose,CGM,blood sugar,diabetes,nightscout,dexcom,libre,time in range,A1c,AGP,GMI,follower,sugar
```
`[97]`

> Don't repeat words already in the app name/subtitle — Apple indexes those
> automatically, so "glucose"/"CGM" here is belt-and-braces. Swap in regional terms
> if useful (e.g. `mmol`).

---

## Description  `[≤4000]`  (currently ~2 480)

```
NightKnight is a fast, private, beautifully clear way to follow continuous glucose
monitoring (CGM) data from your own server. It connects to a NightKnight or
Nightscout-compatible deployment you control — so your glucose history stays yours,
never sold and never mined for ads.

A GLANCEABLE DASHBOARD
• Your current glucose in big, colour-coded type with a live trend arrow and a
  last-hour sparkline.
• A clean 24-hour chart with your target band, high/low lines, and points coloured
  by range. Drag across it to scrub any reading.
• Trailing summary you can flip between 24h, 7, 14, 30 and 90 days: updated GMI
  (uGMI), average glucose, Time in Range and variability (CV), with a stacked
  time-in-range bar.

ANALYSIS THAT ACTUALLY EXPLAINS ITSELF
• Glycemia Risk Index (GRI) with its hypo/hyper breakdown and risk zone.
• Ambulatory Glucose Profile (AGP): every day overlaid into one 24-hour picture with
  median and percentile bands.
• Time-of-day patterns — overnight, morning, afternoon, evening — so you can see when
  things drift.
• Hypo/hyper episode detection with a recent-events feed, plus advanced variability
  (J-Index, MAGE, CONGA, SD).
• Every metric has a plain-language "?" explainer with a link to the clinical source.

BOTH UNITS, FIRST-CLASS
mg/dL and mmol/L are both fully supported. Switch any time; every chart, metric and
alarm follows instantly.

ON-DEVICE ALARMS
Optional high, low and rapid-drop alerts that run on your phone. Simple on/off, no
accounts, nothing to configure in the cloud.

APPLE WATCH & WIDGETS
A glanceable Watch app and complication, plus Home Screen and Lock Screen widgets,
keep your latest number and trend a wrist-flick away.

APPLE HEALTH
Optionally read glucose from Apple Health, or write your readings back to it — you
choose, and you can turn it off any time.

PRIVATE & SECURE BY DESIGN
• Talks only to the server you point it at — your NightKnight or Nightscout instance.
• Credentials are sent in headers, never in the URL.
• Works behind Cloudflare Access with a service token.
• No third-party analytics, no advertising SDKs, no account with us.

WHAT YOU NEED
NightKnight is a follower/viewer app. You need a NightKnight or Nightscout-compatible
server with your CGM data (for example fed by xDrip+, Loop, AndroidAPS, Trio, or a
Dexcom/Libre bridge). Add your server URL and device token in Settings and you're in.

NOT A MEDICAL DEVICE
NightKnight is for information and personal insight only. It is not a medical device
and must not be used as the sole basis for any treatment decision. Always confirm with
your CGM/meter and follow your healthcare provider's guidance.
```

---

## What's New  `[≤4000]`  (1.0 release notes)

```
First release of NightKnight.

• Live glucose dashboard: current value, trend, last-hour sparkline and a scrubbable
  24-hour chart.
• Full statistical analysis: uGMI, Time in Range, GRI, AGP, time-of-day patterns,
  episode detection and advanced variability — each with a plain-language explainer.
• mg/dL and mmol/L both fully supported.
• On-device high/low/rapid-drop alarms.
• Apple Watch app, complication and Home/Lock Screen widgets.
• Optional Apple Health read/write.
• Private by design: connects only to your own NightKnight or Nightscout server.

Thanks for trying NightKnight. Feedback is very welcome.
```

---

## Screenshot captions

Captions to overlay (or use as ASC "alt"/marketing text). Keep ≤ ~6 words so they read
on-device. Same order as the files in `screenshots/iphone-6.9` and `screenshots/ipad-13`.

| # | File | Caption | Sub-caption |
|---|------|---------|-------------|
| 1 | 01-dashboard | **Your glucose, at a glance** | Live value, trend & 24-hour chart |
| 2 | 02-dashboard-mmol | **mg/dL or mmol/L** | Both units, first-class |
| 3 | 03-analysis-overview | **Know your risk** | GRI, uGMI & the metrics that matter |
| 4 | 04-analysis-agp | **See your typical day** | Ambulatory Glucose Profile |
| 5 | 05-analysis-episodes | **Catch every high & low** | Episodes, patterns & variability |
| 6 | 06-settings | **Private by design** | Your server, your data, your rules |

---

## App information

- **Primary category:** Medical  ·  **Secondary:** Health & Fitness
  - (If "Medical" triggers extra review friction, Health & Fitness primary is a valid
    fallback for a data-viewer.)
- **Age rating:** 12+ — select "Infrequent/Mild Medical/Treatment Information" so the
  rating reflects health content honestly.
- **Price:** Free (no IAP, no ads).
- **Support URL:** https://github.com/TGRGIT/NightKnight  *(or a dedicated support page)*
- **Marketing URL:** https://nightknight.cooney.be  *(optional)*
- **Privacy Policy URL:** required — must state that NightKnight stores no data on our
  servers and talks only to the user's own deployment.

## Privacy "nutrition label" (App Privacy)

- **Data collected by the developer:** None. NightKnight has no backend of ours; it
  talks only to the user's server.
- **Health & Fitness › Health:** used **on device** only (display/analysis, optional
  Apple Health sync). Not linked to identity, not used for tracking.
- **No tracking, no third-party analytics, no ads.**

## App Review notes (paste into "Notes")

```
NightKnight is a follower/viewer for a self-hosted NightKnight or Nightscout-compatible
glucose server; it does not connect to CGM hardware directly.

To review without standing up a server, launch the app with demo data:
the build supports a Demo mode that fills every screen with realistic synthetic
readings and analytics (no network). If you need a live demo server or a device token,
contact us at <support email> and we will provision a read-only test instance.

HealthKit is optional and off by default; the app does not require Health access to
run. On-device notifications (alarms) are also optional and off by default.

NightKnight is not a medical device and presents an explicit disclaimer in-app.
```

> Reviewers can't enable Demo mode themselves (it's a debug launch argument), so either
> provide a TestFlight build wired to a seeded server, or include a working server URL +
> read-only device token in these notes. A live, populated instance is the smoothest path
> through review for a data-viewer app.
