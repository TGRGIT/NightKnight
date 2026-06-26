#!/bin/bash
# Capture App Store screenshots from the NightKnight demo build.
# Usage: capture.sh <device-udid> <output-dir>
set -euo pipefail

export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
BID=be.cooney.nightknight.NightKnight
IOS_DIR="$(cd "$(dirname "$0")/../../ios" && pwd)"
APP="$IOS_DIR/build/DerivedData/Build/Products/Debug-iphonesimulator/NightKnight.app"

UDID="$1"; OUT="$2"
mkdir -p "$OUT"

xcrun simctl bootstatus "$UDID" -b >/dev/null 2>&1 || true
xcrun simctl install "$UDID" "$APP"
# Clean marketing status bar: 9:41, full battery / signal.
xcrun simctl status_bar "$UDID" override \
  --time "9:41" --batteryState charged --batteryLevel 100 \
  --cellularBars 4 --wifiBars 3 --dataNetwork wifi

# name | NK_* env (space separated)
shots=(
  "01-dashboard|NK_UNIT=mgdl NK_PERIOD=7 NK_TAB=0"
  "02-dashboard-mmol|NK_UNIT=mmol NK_PERIOD=14 NK_TAB=0 NK_ALARMS=1"
  "03-analysis-overview|NK_UNIT=mgdl NK_TAB=1 NK_SCROLL=gri"
  "04-analysis-agp|NK_UNIT=mgdl NK_TAB=1 NK_SCROLL=agp"
  "05-analysis-episodes|NK_UNIT=mgdl NK_TAB=1 NK_SCROLL=episodes"
  "06-settings|NK_UNIT=mgdl NK_TAB=2 NK_ALARMS=1"
)

for s in "${shots[@]}"; do
  name="${s%%|*}"
  envs="${s#*|}"
  # Turn "NK_UNIT=mgdl NK_PERIOD=7" into SIMCTL_CHILD_ exports.
  child=("SIMCTL_CHILD_NK_DEMO=1")
  for kv in $envs; do child+=("SIMCTL_CHILD_$kv"); done

  xcrun simctl terminate "$UDID" "$BID" >/dev/null 2>&1 || true
  env "${child[@]}" xcrun simctl launch "$UDID" "$BID" -NKDemo >/dev/null
  sleep 4.5
  tmp="$(mktemp -t nkshot).png"
  xcrun simctl io "$UDID" screenshot "$tmp" >/dev/null 2>&1
  # Flatten any alpha channel — App Store Connect rejects screenshots with alpha.
  ffmpeg -y -loglevel error -i "$tmp" -frames:v 1 -pix_fmt rgb24 "$OUT/$name.png"
  rm -f "$tmp"
  echo "  $name.png  ($(sips -g pixelWidth -g pixelHeight "$OUT/$name.png" | awk '/pixel/{print $2}' | paste -sd'x' -))"
done

xcrun simctl terminate "$UDID" "$BID" >/dev/null 2>&1 || true
echo "Done: $OUT"
