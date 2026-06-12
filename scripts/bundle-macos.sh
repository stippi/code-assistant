#!/usr/bin/env bash
# Build a macOS .app bundle for Code Assistant.
#
# Uses the static Info.plist from assets/ and replaces only the version.
# Only relies on tools that ship with macOS (plutil, codesign).
#
# Usage:
#   ./scripts/bundle-macos.sh                   # build for the host arch
#   ./scripts/bundle-macos.sh --no-build ARCH   # reuse existing binary
#
# ARCH can be: aarch64, x86_64, universal
#
# Output:
#   target/macos-bundle/Code Assistant.app
#   target/macos-bundle/Code-Assistant-<version>-<arch>.zip

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
    --no-build)    DO_BUILD=0 ;;
    aarch64|arm64) ARCH="aarch64" ;;
    x86_64|intel)  ARCH="x86_64" ;;
    universal)     ARCH="universal" ;;
    -h|--help)     sed -n '2,16p' "$0"; exit 0 ;;
    *)             echo "error: unknown argument: $arg" >&2; exit 1 ;;
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
# Paths
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CRATE_DIR="$REPO_ROOT/crates/code_assistant"
ASSETS_DIR="$CRATE_DIR/assets"

VERSION="$(grep -m 1 '^version = ' "$CRATE_DIR/Cargo.toml" | sed 's/version = "\(.*\)"/\1/')"
EXECUTABLE_NAME="code-assistant"

OUT_DIR="$REPO_ROOT/target/macos-bundle"
APP_DIR="$OUT_DIR/Code Assistant.app"
CONTENTS="$APP_DIR/Contents"

echo "==> Bundling Code Assistant v$VERSION ($ARCH)"

# ---------------------------------------------------------------------------
# Build
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

# Fallback: accept target/release/code-assistant when --no-build is used
if [[ ! -x "$SRC_BIN" && $DO_BUILD -eq 0 && -x "$REPO_ROOT/target/release/$EXECUTABLE_NAME" ]]; then
  echo "==> Using fallback binary at target/release/$EXECUTABLE_NAME"
  SRC_BIN="$REPO_ROOT/target/release/$EXECUTABLE_NAME"
fi

if [[ ! -x "$SRC_BIN" ]]; then
  echo "error: binary not found at $SRC_BIN" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Assemble the .app bundle
# ---------------------------------------------------------------------------
echo "==> Creating bundle at $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"

cp "$SRC_BIN" "$CONTENTS/MacOS/$EXECUTABLE_NAME"
chmod +x "$CONTENTS/MacOS/$EXECUTABLE_NAME"

# Icon
ICNS_SRC="$ASSETS_DIR/AppIcon.icns"
if [[ ! -f "$ICNS_SRC" ]]; then
  echo "==> Regenerating AppIcon.icns"
  "$SCRIPT_DIR/generate-app-icon.sh"
fi
cp "$ICNS_SRC" "$CONTENTS/Resources/AppIcon.icns"

# Info.plist — substitute version into the static template
sed "s/\${VERSION}/$VERSION/g" "$ASSETS_DIR/Info.plist" > "$CONTENTS/Info.plist"
plutil -lint "$CONTENTS/Info.plist" >/dev/null

printf 'APPL????' > "$CONTENTS/PkgInfo"
touch "$APP_DIR"

# ---------------------------------------------------------------------------
# Ad-hoc code signature
# ---------------------------------------------------------------------------
if command -v codesign >/dev/null 2>&1; then
  echo "==> Ad-hoc signing"
  codesign --force --deep --sign - "$APP_DIR" >/dev/null 2>&1 || true
fi

# ---------------------------------------------------------------------------
# Zip for distribution
# ---------------------------------------------------------------------------
ZIP_PATH="$OUT_DIR/Code-Assistant-$VERSION-$ARCH.zip"
rm -f "$ZIP_PATH"
( cd "$OUT_DIR" && zip -qry "$ZIP_PATH" "Code Assistant.app" )

echo
echo "==> Done:"
echo "    $APP_DIR"
echo "    $ZIP_PATH"
