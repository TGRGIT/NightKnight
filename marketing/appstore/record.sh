#!/bin/bash
# Record an App Preview from the NightKnight autoplay demo and conform it to Apple's
# App Preview spec (resolution, 30fps, H.264, required stereo AAC audio track).
# Usage: record.sh <device-udid> <scaleWxH e.g. 886:1920> <output.mp4>
set -euo pipefail

export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
BID=be.cooney.nightknight.NightKnight
IOS_DIR="$(cd "$(dirname "$0")/../../ios" && pwd)"
APP="$IOS_DIR/build/DerivedData/Build/Products/Debug-iphonesimulator/NightKnight.app"

UDID="$1"; SCALE="$2"; OUT="$3"
mkdir -p "$(dirname "$OUT")"

xcrun simctl bootstatus "$UDID" -b >/dev/null 2>&1 || true
xcrun simctl install "$UDID" "$APP"
xcrun simctl status_bar "$UDID" override \
  --time "9:41" --batteryState charged --batteryLevel 100 \
  --cellularBars 4 --wifiBars 3 --dataNetwork wifi
xcrun simctl terminate "$UDID" "$BID" >/dev/null 2>&1 || true

# Launch FIRST and let the app fully render (avoids a black launch frame), THEN record.
# The selector sweeps loop continuously, so there's motion whenever recording starts.
env SIMCTL_CHILD_NK_DEMO=1 SIMCTL_CHILD_NK_AUTOPLAY=1 \
    SIMCTL_CHILD_NK_TAB=0 SIMCTL_CHILD_NK_UNIT=mgdl SIMCTL_CHILD_NK_PERIOD=7 \
    xcrun simctl launch "$UDID" "$BID" -NKDemo >/dev/null
sleep 4
raw="$(mktemp -t nkrec).mov"
xcrun simctl io "$UDID" recordVideo --codec h264 --force "$raw" &
REC=$!
sleep 24
kill -INT "$REC" 2>/dev/null || true
wait "$REC" 2>/dev/null || true
xcrun simctl terminate "$UDID" "$BID" >/dev/null 2>&1 || true

# Conform to App Preview spec: exact resolution, 30fps, H.264 yuv420p, and a SILENT but
# present stereo AAC track (App Store Connect requires an audio track). Trim to 24s.
ffmpeg -y -loglevel error -i "$raw" \
  -f lavfi -t 24 -i anullsrc=channel_layout=stereo:sample_rate=44100 \
  -map 0:v:0 -map 1:a:0 -t 24 \
  -vf "scale=${SCALE}:flags=lanczos,fps=30,format=yuv420p" \
  -c:v libx264 -profile:v high -pix_fmt yuv420p -b:v 12M -maxrate 16M -bufsize 24M \
  -c:a aac -b:a 256k -ar 44100 -ac 2 \
  -movflags +faststart "$OUT"
rm -f "$raw"

echo "wrote $OUT"
echo -n "  video: "; ffprobe -v error -select_streams v:0 -show_entries stream=width,height,r_frame_rate -of csv=p=0 "$OUT"
echo -n "  audio: "; ffprobe -v error -select_streams a:0 -show_entries stream=codec_name,channels,sample_rate -of csv=p=0 "$OUT"
echo -n "  dur:   "; ffprobe -v error -show_entries format=duration:format=size -of csv=p=0 "$OUT"
