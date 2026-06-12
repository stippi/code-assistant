#!/usr/bin/env bash
# Regenerate the macOS app icon (AppIcon.icns) from app_icon.svg.
#
# Run this after editing crates/code_assistant/assets/app_icon.svg.
#
# Requires: macOS (sips + iconutil).

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: generate-app-icon.sh must be run on macOS" >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ASSETS_DIR="$SCRIPT_DIR/../crates/code_assistant/assets"
cd "$ASSETS_DIR"

if [[ ! -f app_icon.svg ]]; then
  echo "error: app_icon.svg not found in $ASSETS_DIR" >&2
  exit 1
fi

echo "==> Rendering app_icon.svg -> app_icon.png (1024x1024)"
sips -s format png -Z 1024 app_icon.svg --out app_icon.png > /dev/null

echo "==> Building AppIcon.iconset"
rm -rf AppIcon.iconset
mkdir AppIcon.iconset

sips -z 16    16    app_icon.png --out AppIcon.iconset/icon_16x16.png       > /dev/null
sips -z 32    32    app_icon.png --out AppIcon.iconset/icon_16x16@2x.png    > /dev/null
sips -z 32    32    app_icon.png --out AppIcon.iconset/icon_32x32.png       > /dev/null
sips -z 64    64    app_icon.png --out AppIcon.iconset/icon_32x32@2x.png    > /dev/null
sips -z 128   128   app_icon.png --out AppIcon.iconset/icon_128x128.png     > /dev/null
sips -z 256   256   app_icon.png --out AppIcon.iconset/icon_128x128@2x.png  > /dev/null
sips -z 256   256   app_icon.png --out AppIcon.iconset/icon_256x256.png     > /dev/null
sips -z 512   512   app_icon.png --out AppIcon.iconset/icon_256x256@2x.png  > /dev/null
sips -z 512   512   app_icon.png --out AppIcon.iconset/icon_512x512.png     > /dev/null
cp app_icon.png             AppIcon.iconset/icon_512x512@2x.png

echo "==> Converting iconset to AppIcon.icns"
iconutil -c icns AppIcon.iconset -o AppIcon.icns

# Clean up the intermediate iconset
rm -rf AppIcon.iconset

echo "==> Done:"
ls -la app_icon.svg app_icon.png AppIcon.icns
