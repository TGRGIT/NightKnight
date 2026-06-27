# NightKnight — App Store assets

Screenshots, App Previews and listing copy for the NightKnight iOS app, captured from
the real app (branch `feat/analytics-importers-ios-overhaul`) running in the iOS
Simulator with a DEBUG-only demo-data mode.

## Contents

```
marketing/appstore/
  APP-STORE-COPY.md              name, subtitle, promo, description, keywords, captions, review notes
  screenshots/
    iphone-6.9/    01..06.png    1320 × 2868  (iPhone 16 Pro Max — 6.9")
    ipad-13/       01..06.png    2064 × 2752  (iPad Pro 13" M4 — 13")
    watch-series11/ 01..02.png   416 × 496    (Apple Watch Series 11 46mm)
    watch-ultra3/   01..02.png   422 × 514    (Apple Watch Ultra 3 49mm)
    web/            01..02.png   1440 × 900   (web UI, for README / docs)
  previews/
    iphone-6.9-preview.mp4       886 × 1920, H.264, 30 fps, stereo AAC, 24 s
    ipad-13-preview.mp4          1200 × 1600, H.264, 30 fps, stereo AAC, 24 s
  capture.sh                     regenerate iPhone/iPad screenshot set
  record.sh                      regenerate an App Preview
```

The six shots, in order: Dashboard (mg/dL) · Dashboard (mmol/L, alarms on) ·
Analysis overview (GRI + core metrics) · AGP · Episodes & variability · Settings.

## Apple spec compliance

| Asset | Apple requirement | These files |
|-------|-------------------|-------------|
| iPhone 6.9" screenshot | 1320 × 2868 portrait, PNG/JPEG, no alpha | 1320 × 2868 PNG, alpha stripped (`-pix_fmt rgb24`) ✓ |
| iPad 13" screenshot | 2064 × 2752 portrait, PNG/JPEG, no alpha | 2064 × 2752 PNG, alpha stripped ✓ |
| Watch Ultra 3 screenshot | 422 × 514 or 410 × 502, PNG/JPEG, no alpha | 422 × 514 PNG, alpha stripped ✓ |
| Watch Series 11 screenshot | 416 × 496, PNG/JPEG, no alpha | 416 × 496 PNG, alpha stripped ✓ |
| iPhone 6.9" preview | 886 × 1920, H.264, 30 fps, 15–30 s, **stereo AAC audio**, ≤500 MB | 886 × 1920, 30 fps, 24 s, AAC 256k/44.1kHz stereo, ~4 MB ✓ |
| iPad 13" preview | 1200 × 1600, H.264, 30 fps, 15–30 s, **stereo AAC audio**, ≤500 MB | 1200 × 1600, 30 fps, 24 s, AAC 256k/44.1kHz stereo, ~6 MB ✓ |

Notes:
- The status bar is overridden to the marketing-standard **9:41**, full battery/signal.
- App Store Connect requires an **audio track** on previews even if silent; both videos
  carry a silent stereo AAC track (no licensing risk). Swap in a soundtrack later if you
  want, keeping the same a/v settings.
- Apple now only *requires* the 6.9" iPhone and 13" iPad sizes; it down-scales these for
  older device families automatically.

## How the data is generated (DEBUG demo mode)

The app is data-driven from a server, so a small **DEBUG-only** layer
(`ios/Shared/DemoData.swift`) feeds realistic, deterministic synthetic data when
launched with `-NKDemo`. It is injected at the `APIClient` layer, so every tab, the
widget and the watch render identically with **no network**. It is compiled out of
release builds (`#if DEBUG`).

Launch arguments / env (passed to the sim as `SIMCTL_CHILD_NK_*`):

| var | effect |
|-----|--------|
| `NK_DEMO=1` / `-NKDemo` | enable demo mode |
| `NK_UNIT=mgdl\|mmol` | display unit |
| `NK_PERIOD=1\|7\|14\|30\|90` | dashboard trailing period |
| `NK_TAB=0\|1\|2` | start on Dashboard / Analysis / Settings |
| `NK_SCROLL=gri\|core\|agp\|tod\|episodes\|advanced` | scroll Analysis to a section |
| `NK_ALARMS=1` | show alarms enabled |
| `NK_AUTOPLAY=1` | sweep selectors + cycle tabs (for recording) |

## Regenerate

```bash
# Build the demo app for the simulator (Xcode required; uses full Xcode, not CLT):
cd ios && xcodegen generate
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcrun xcodebuild \
  -project NightKnight.xcodeproj -scheme NightKnight -configuration Debug \
  -destination 'name=iPhone 16 Pro Max' -derivedDataPath build/DerivedData \
  CODE_SIGNING_ALLOWED=NO build

# Screenshots (boot the sims first):
marketing/appstore/capture.sh <iphone-16-pro-max-udid> marketing/appstore/screenshots/iphone-6.9
marketing/appstore/capture.sh <ipad-pro-13-udid>       marketing/appstore/screenshots/ipad-13

# Watch screenshots — capture.sh doesn't cover watch; use simctl directly:
# (capture.sh handles status_bar override which is unsupported on watchOS)
BID=be.cooney.nightknight.NightKnight.watchkitapp
WATCH_APP=ios/build/DerivedData/Build/Products/Debug-watchsimulator/NightKnightWatch.app
for UDID in <series-11-46mm-udid> <ultra3-49mm-udid>; do
  xcrun simctl install $UDID $WATCH_APP
  for UNIT in mgdl mmol; do
    xcrun simctl terminate $UDID $BID 2>/dev/null || true
    SIMCTL_CHILD_NK_DEMO=1 SIMCTL_CHILD_NK_UNIT=$UNIT xcrun simctl launch $UDID $BID -NKDemo >/dev/null
    sleep 5
    tmp=$(mktemp).png
    xcrun simctl io $UDID screenshot $tmp
    ffmpeg -y -loglevel error -i $tmp -frames:v 1 -pix_fmt rgb24 marketing/appstore/screenshots/watch-<size>/$UNIT.png
  done
done

# App Previews:
marketing/appstore/record.sh <iphone-udid> 886:1920  marketing/appstore/previews/iphone-6.9-preview.mp4
marketing/appstore/record.sh <ipad-udid>   1200:1600 marketing/appstore/previews/ipad-13-preview.mp4
```

`xcrun simctl list devices` shows the UDIDs. The same simulator `.app` runs on both
iPhone and iPad simulators.
