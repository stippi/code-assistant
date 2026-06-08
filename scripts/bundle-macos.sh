#!/usr/bin/env bash
# Build a macOS .app bundle for code-assistant.
#
# This is a self-contained alternative to `cargo bundle`. It only relies on
# tools that ship with macOS (sips, iconutil, plutil) and on the project's
# checked-in icon assets.
#
# Usage:
#   ./scripts/bundle-macos.sh                  # build for the host arch
#   ./scripts/bundle-macos.sh aarch64          # Apple Silicon
#   ./scripts/bundle-macos.sh x86_64           # Intel
#   ./scripts/bundle-macos.sh universal        # universal (lipo) bundle
#   ./scripts/bundle-macos.sh --no-build aarch64
#       Reuse an already-built binary (target/<triple>/release/code-assistant).
#
# Output:
#   target/macos-bundle/Code Assistant.app
#   target/macos-bundle/Code Assistant.zip   (zipped bundle)

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: bundle-macos.sh must be run on macOS" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
DO_BUILD=1
ARCH=""
for arg in "$@"; do
  case "$arg" in
    --no-build)   DO_BUILD=0 ;;
    aarch64|arm64) ARCH="aarch64" ;;
    x86_64|intel)  ARCH="x86_64" ;;
    universal)     ARCH="universal" ;;
    -h|--help)
      sed -n '2,30p' "$0"
      exit 0
      ;;
    *)
      echo "error: unknown argument: $arg" >&2
      exit 1
      ;;
  esac
done

if [[ -z "$ARCH" ]]; then
  case "$(uname -m)" in
    arm64)  ARCH="aarch64" ;;
    x86_64) ARCH="x86_64"  ;;
    *) echo "error: unsupported host arch: $(uname -m)" >&2; exit 1 ;;
  esac
fi

# ---------------------------------------------------------------------------
# Paths and metadata
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CRATE_DIR="$REPO_ROOT/crates/code_assistant"
ASSETS_DIR="$CRATE_DIR/assets"

VERSION="$(grep -m 1 '^version = ' "$CRATE_DIR/Cargo.toml" | sed 's/version = "\(.*\)"/\1/')"
APP_NAME="Code Assistant"
BUNDLE_ID="dev.stippi.code-assistant"
EXECUTABLE_NAME="code-assistant"

OUT_DIR="$REPO_ROOT/target/macos-bundle"
APP_DIR="$OUT_DIR/$APP_NAME.app"
CONTENTS="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS/MacOS"
RES_DIR="$CONTENTS/Resources"

echo "==> Bundling $APP_NAME v$VERSION ($ARCH)"

# ---------------------------------------------------------------------------
# Build (unless --no-build)
# ---------------------------------------------------------------------------
build_target() {
  local triple="$1"
  echo "==> cargo build --release --target $triple"
  rustup target add "$triple" >/dev/null 2>&1 || true
  ( cd "$REPO_ROOT" && cargo build --locked --release --target "$triple" -p code-assistant )
}

case "$ARCH" in
  aarch64)
    TRIPLE="aarch64-apple-darwin"
    [[ $DO_BUILD -eq 1 ]] && build_target "$TRIPLE"
    SRC_BIN="$REPO_ROOT/target/$TRIPLE/release/$EXECUTABLE_NAME"
    ;;
  x86_64)
    TRIPLE="x86_64-apple-darwin"
    [[ $DO_BUILD -eq 1 ]] && build_target "$TRIPLE"
    SRC_BIN="$REPO_ROOT/target/$TRIPLE/release/$EXECUTABLE_NAME"
    ;;
  universal)
    if [[ $DO_BUILD -eq 1 ]]; then
      build_target "aarch64-apple-darwin"
      build_target "x86_64-apple-darwin"
    fi
    UNIVERSAL_DIR="$REPO_ROOT/target/universal-apple-darwin/release"
    mkdir -p "$UNIVERSAL_DIR"
    lipo -create -output "$UNIVERSAL_DIR/$EXECUTABLE_NAME" \
      "$REPO_ROOT/target/aarch64-apple-darwin/release/$EXECUTABLE_NAME" \
      "$REPO_ROOT/target/x86_64-apple-darwin/release/$EXECUTABLE_NAME"
    SRC_BIN="$UNIVERSAL_DIR/$EXECUTABLE_NAME"
    ;;
esac

# Fallback: when running with --no-build and no triple-specific binary
# exists, accept a default `target/release/code-assistant` if it matches the
# requested arch. This makes the script convenient on a developer's machine
# where `cargo build --release` was already run without an explicit --target.
if [[ ! -x "$SRC_BIN" && $DO_BUILD -eq 0 && -x "$REPO_ROOT/target/release/$EXECUTABLE_NAME" ]]; then
  HOST_ARCH=""
  case "$(uname -m)" in
    arm64)  HOST_ARCH="aarch64" ;;
    x86_64) HOST_ARCH="x86_64"  ;;
  esac
  if [[ "$ARCH" == "$HOST_ARCH" || "$ARCH" == "universal" ]]; then
    echo "==> Using fallback binary at target/release/$EXECUTABLE_NAME"
    SRC_BIN="$REPO_ROOT/target/release/$EXECUTABLE_NAME"
  fi
fi

if [[ ! -x "$SRC_BIN" ]]; then
  echo "error: binary not found at $SRC_BIN" >&2
  echo "       (re-run without --no-build, or build manually first)" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Layout the .app bundle
# ---------------------------------------------------------------------------
echo "==> Creating bundle skeleton at $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RES_DIR"

# Binary
cp "$SRC_BIN" "$MACOS_DIR/$EXECUTABLE_NAME"
chmod +x "$MACOS_DIR/$EXECUTABLE_NAME"

# Icon
ICNS_SRC="$ASSETS_DIR/AppIcon.icns"
if [[ ! -f "$ICNS_SRC" ]]; then
  echo "==> AppIcon.icns missing, regenerating from app_icon.svg"
  "$SCRIPT_DIR/generate-app-icon.sh"
fi
cp "$ICNS_SRC" "$RES_DIR/AppIcon.icns"

# Info.plist
echo "==> Writing Info.plist"
cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleExecutable</key>
    <string>$EXECUTABLE_NAME</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundleIconName</key>
    <string>AppIcon</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>LSMinimumSystemVersion</key>
    <string>10.15</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>Code Assistant contributors</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST

# Validate & normalize plist (binary form is preferred, but XML works fine too).
plutil -lint "$CONTENTS/Info.plist" >/dev/null

# PkgInfo helps older Finder / Launch Services recognise the app.
printf 'APPL????' > "$CONTENTS/PkgInfo"

# Refresh icon cache for the bundle so Finder picks up the new icon
# (no-op when not running interactively, harmless otherwise).
touch "$APP_DIR"

# ---------------------------------------------------------------------------
# Optional ad-hoc code signature.
# Distributing outside the App Store still requires a Developer ID signature,
# but ad-hoc signing makes the bundle launchable on the build machine and
# avoids "is damaged" errors on first run.
# ---------------------------------------------------------------------------
if command -v codesign >/dev/null 2>&1; then
  echo "==> Ad-hoc signing the bundle"
  codesign --force --deep --sign - "$APP_DIR" >/dev/null 2>&1 || true
fi

# ---------------------------------------------------------------------------
# Zip the bundle for distribution
# ---------------------------------------------------------------------------
ZIP_PATH="$OUT_DIR/${APP_NAME// /-}-$VERSION-$ARCH.zip"
rm -f "$ZIP_PATH"
( cd "$OUT_DIR" && zip -qry "$ZIP_PATH" "$APP_NAME.app" )

echo
echo "==> Bundle ready:"
echo "    $APP_DIR"
echo "    $ZIP_PATH"
