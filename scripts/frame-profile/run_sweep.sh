#!/usr/bin/env bash
# Run one profiled sweep of a code-assistant release binary through a
# throwaway .app bundle, so LaunchServices activates the window (gpui draws
# nothing while the window is occluded). See docs/frame-profiling.md.
#
# Usage: run_sweep.sh NAME BINARY MODE [SAMPLE_AT [SAMPLE_SECS]]
#   NAME         label for the output files, e.g. before-scroll-1
#   BINARY       the release binary to run (copy it first when comparing builds)
#   MODE         scroll | wheel | 1   (CODE_ASSISTANT_FRAME_PROFILE)
#   SAMPLE_AT    seconds after launch to attach `sample` (optional; it
#                inflates the draw times of those intervals)
#   SAMPLE_SECS  sample duration, default 8
#
# Environment:
#   CODE_ASSISTANT_DATA_DIR  the session copy to load (required)
#   FRAME_PROFILE_OUT        output directory, default ./frame-profile-runs
#
# Output: $FRAME_PROFILE_OUT/NAME.stderr.log (the reports, feed to
# report_agg.py), NAME.sample.txt and the demangled NAME.dem.txt (feed to
# sample_phase.py).
set -euo pipefail
NAME="$1"; BINARY="$2"; MODE="$3"; SAMPLE_AT="${4:-}"; SAMPLE_SECS="${5:-8}"
: "${CODE_ASSISTANT_DATA_DIR:?set CODE_ASSISTANT_DATA_DIR to the session copy}"
OUT="${FRAME_PROFILE_OUT:-./frame-profile-runs}"
mkdir -p "$OUT"
APP="$OUT/app/$NAME/Code Assistant Perf.app"
rm -rf "$OUT/app/$NAME"
mkdir -p "$APP/Contents/MacOS"
cp "$BINARY" "$APP/Contents/MacOS/code-assistant"
cat > "$APP/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Code Assistant Perf</string>
<key>CFBundleIdentifier</key><string>dev.stippi.code-assistant.perf</string>
<key>CFBundleVersion</key><string>0.0.1</string>
<key>CFBundleShortVersionString</key><string>0.0.1</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleExecutable</key><string>code-assistant</string>
<key>LSMinimumSystemVersion</key><string>10.15</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
EOF
codesign --force --sign - "$APP" >/dev/null 2>&1 || true

LOG="$OUT/$NAME.stderr.log"; : > "$LOG"
open -n -a "$APP" --env CODE_ASSISTANT_DATA_DIR="$CODE_ASSISTANT_DATA_DIR" \
  --env CODE_ASSISTANT_FRAME_PROFILE="$MODE" --stderr "$LOG" --stdout "$OUT/$NAME.stdout.log"
EXE="$APP/Contents/MacOS/code-assistant"
PID=""
for _ in $(seq 1 50); do
  PID="$(pgrep -n -f "$EXE" || true)"
  [[ -n "$PID" ]] && break
  sleep 0.2
done
[[ -z "$PID" ]] && { echo "app did not start" >&2; exit 1; }
echo "pid=$PID name=$NAME mode=$MODE"
START=$(date +%s)
if [[ -n "$SAMPLE_AT" ]]; then
  sleep "$SAMPLE_AT"
  sample "$PID" "$SAMPLE_SECS" -mayDie -file "$OUT/$NAME.sample.txt" >/dev/null 2>&1 || echo "sample failed" >&2
  [[ -f "$OUT/$NAME.sample.txt" ]] && c++filt < "$OUT/$NAME.sample.txt" > "$OUT/$NAME.dem.txt"
fi
# 8 s settle + 40 s sweep + margin
while [[ $(( $(date +%s) - START )) -lt 52 ]]; do sleep 1; done
kill "$PID" 2>/dev/null || true
sleep 1
kill -9 "$PID" 2>/dev/null || true
echo "done: $LOG"
