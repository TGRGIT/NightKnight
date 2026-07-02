#!/bin/bash
# Record an App Preview by driving the demo autoplay walk with the dedicated
# XCUITest (NightKnightUITests/AppStorePreviewUITests) and conforming the capture to
# Apple's App Preview spec: exact resolution, 30 fps, H.264 yuv420p, 15–30 s, and
# VIDEO-ONLY — no audio track (a synthetic silent track adds nothing and App Store
# Connect accepts audio-less previews).
#
# The capture window is MARKER-DRIVEN, not timing-guessed: the test prints
# NKPREVIEW_READY right when it has a clean opening frame and NKPREVIEW_DONE right
# when the closing frame is ready, and this script starts/stops `simctl io
# recordVideo` on those two lines. A fixed "sleep after pgrep, record for N seconds"
# schedule was tried first and is fragile — app launch time (RustAnalytics's ABI
# assert, the legacy-credential migration, WatchConnectivity startup, SwiftUI scene
# setup) varies a lot with host load, so a flat pre-roll sleep can start recording
# before the UI is ready, and a flat recording duration can end the capture mid-tour
# on a slow run. Marker lines make both edges exact regardless of how fast this
# particular run is.
#
# Usage: record.sh <device-udid> <scaleWxH e.g. 886:1920> <output.mp4>
set -euo pipefail

export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
BID=be.cooney.nightknight.NightKnight
IOS_DIR="$(cd "$(dirname "$0")/../../ios" && pwd)"

UDID="$1"; SCALE="$2"; OUT="$3"
mkdir -p "$(dirname "$OUT")"

xcrun simctl bootstatus "$UDID" -b >/dev/null 2>&1 || true
xcrun simctl status_bar "$UDID" override \
  --time "9:41" --batteryState charged --batteryLevel 100 \
  --cellularBars 4 --wifiBars 3 --dataNetwork wifi
xcrun simctl terminate "$UDID" "$BID" >/dev/null 2>&1 || true
pkill -x NightKnight 2>/dev/null || true

# Drive the walk with the XCUITest, piped through a pty (`script`) so xcodebuild's
# stdout — including the test's NKPREVIEW_* print() markers — is line-buffered and
# shows up in $log as it happens, instead of being fully buffered (and delayed until
# process exit) the way redirecting straight to a file would leave it.
log="$(mktemp -t nk-preview-test).log"
(cd "$IOS_DIR" && script -q "$log" xcodebuild test -scheme NightKnight \
    -destination "platform=iOS Simulator,id=$UDID" \
    -only-testing:NightKnightUITests/AppStorePreviewUITests/testAppStorePreviewWalk \
    CODE_SIGNING_ALLOWED=NO) >/dev/null 2>&1 &
XCB=$!

# Poll $log for a marker line. Fails loudly (rather than falling back to a guess) if
# the test process dies first or the marker never shows up within `timeout` seconds.
wait_for_marker() {
    local marker="$1" timeout_s="$2" waited=0
    while ! grep -q "$marker" "$log" 2>/dev/null; do
        kill -0 "$XCB" 2>/dev/null || { echo "xcodebuild test exited before $marker; see $log" >&2; return 1; }
        sleep 0.2
        waited=$((waited + 1))
        if [ "$waited" -ge "$((timeout_s * 5))" ]; then
            echo "timed out after ${timeout_s}s waiting for $marker; see $log" >&2
            return 1
        fi
    done
}

# Generous: build + install + a cold simulator launch can legitimately take over a
# minute under load, and this wait covers all of it (recording hasn't started yet).
wait_for_marker "NKPREVIEW_READY" 180 || exit 1

raw="$(mktemp -t nkrec).mov"
xcrun simctl io "$UDID" recordVideo --codec h264 --force "$raw" &
REC=$!
# Let `recordVideo` actually attach before the very next frame renders, so the raw
# capture's first frame is the same clean opening frame NKPREVIEW_READY fired on.
sleep 0.6

# The tour itself is a fixed ~31s budget (10s + 9s + 6s + a final 6s hold) plus
# whatever margin XCUITest's own polling adds — bounded generously here so a
# pathologically slow run fails loudly instead of recording forever.
if ! wait_for_marker "NKPREVIEW_DONE" 90; then
    kill -INT "$REC" 2>/dev/null || true
    wait "$REC" 2>/dev/null || true
    exit 1
fi
kill -INT "$REC" 2>/dev/null || true
wait "$REC" 2>/dev/null || true
wait "$XCB" || { echo "xcodebuild test failed; see $log" >&2; exit 1; }

# Conform to the App Preview spec: exact resolution, 30 fps, H.264 High yuv420p,
# faststart, no audio (-an). The raw capture is already bounded to the real tour
# (marker-to-marker) rather than a blind fixed window, so trimming here only needs
# to (a) skip a fractional-second recorder-startup blip and (b) cap at Apple's 30 s
# ceiling — it does not need to guess where the content actually is.
ffmpeg -y -loglevel error -i "$raw" \
  -ss 0.3 -t 29 -an \
  -vf "scale=${SCALE}:flags=lanczos,fps=30,format=yuv420p" \
  -c:v libx264 -profile:v high -pix_fmt yuv420p -b:v 12M -maxrate 16M -bufsize 24M \
  -movflags +faststart "$OUT"
rm -f "$raw"

echo "wrote $OUT"
echo -n "  video: "; ffprobe -v error -select_streams v:0 -show_entries stream=width,height,r_frame_rate -of csv=p=0 "$OUT"
audio="$(ffprobe -v error -select_streams a -show_entries stream=codec_name -of csv=p=0 "$OUT")"
if [ -n "$audio" ]; then echo "  ERROR: unexpected audio stream ($audio)" >&2; exit 1; fi
echo "  audio: none ✓"
dur="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$OUT")"
echo "  dur:   ${dur}s"
awk -v d="$dur" 'BEGIN { if (d < 15 || d > 30) { print "  ERROR: duration " d "s is outside Apple'"'"'s 15-30s App Preview range" > "/dev/stderr"; exit 1 } }'
