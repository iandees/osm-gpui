#!/usr/bin/env bash
# Build "osm-gpui.app" from an already-built release binary and ad-hoc sign it.
#
# Usage:
#   cargo build --release
#   scripts/bundle-macos.sh [--binary PATH] [--out DIR]
#
# --binary  the executable to package (default: $CARGO_TARGET_DIR/release/osm-gpui,
#           or target/release/osm-gpui). Pass a lipo'd universal binary here.
# --out     directory the .app is created in (default: ./dist)
#
# By default the bundle is only ad-hoc signed, so it is not notarized: on first
# launch macOS asks for right-click > Open. To sign with a Developer ID
# certificate installed in your keychain, set MACOS_SIGNING_IDENTITY, e.g.
#   MACOS_SIGNING_IDENTITY="Developer ID Application: Name (TEAMID)" \
#     scripts/bundle-macos.sh
# This signs with the hardened runtime; notarizing is a separate step (see the
# README, "Signing and notarizing macOS builds").
set -euo pipefail

APP_NAME="osm-gpui"
BUNDLE_ID="com.iandees.osm-gpui"
MIN_MACOS="11.0"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINARY="${CARGO_TARGET_DIR:-$ROOT/target}/release/osm-gpui"
OUT="$ROOT/dist"
# "-" means ad-hoc.
SIGN_IDENTITY="${MACOS_SIGNING_IDENTITY:--}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) BINARY="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ ! -x "$BINARY" ]]; then
  echo "binary not found: $BINARY (run 'cargo build --release' first)" >&2
  exit 1
fi

# Single source of truth for the version: the package version in Cargo.toml.
VERSION="$(sed -n '/^\[package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' "$ROOT/Cargo.toml" | head -n1)"
if [[ -z "$VERSION" ]]; then
  echo "could not read version from Cargo.toml" >&2
  exit 1
fi

APP="$OUT/$APP_NAME.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BINARY" "$APP/Contents/MacOS/$APP_NAME"
chmod +x "$APP/Contents/MacOS/$APP_NAME"

# All runtime data (presets, fields, MapCSS, locales) is embedded in the binary
# at compile time, so Resources/ only holds optional extras such as an icon.
ICON_KEY=""
if [[ -f "$ROOT/assets/AppIcon.icns" ]]; then
  cp "$ROOT/assets/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
  ICON_KEY="  <key>CFBundleIconFile</key>
  <string>AppIcon</string>"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
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
  <key>CFBundleExecutable</key>
  <string>$APP_NAME</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleShortVersionString</key>
  <string>$VERSION</string>
  <key>CFBundleVersion</key>
  <string>$VERSION</string>
  <key>LSMinimumSystemVersion</key>
  <string>$MIN_MACOS</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key>
  <true/>
$ICON_KEY
</dict>
</plist>
PLIST

plutil -lint "$APP/Contents/Info.plist"

if [[ "$SIGN_IDENTITY" == "-" ]]; then
  # Ad-hoc sign so the bundle launches on Apple Silicon.
  codesign --force --deep --sign - "$APP"
else
  # Developer ID signing for distribution: hardened runtime plus a secure
  # timestamp, both required for notarization. Nested code is signed before
  # the bundle (today that is only the main executable; --deep is avoided
  # because Apple recommends signing each piece explicitly). No entitlements
  # are needed: the app is not sandboxed and its network, file and Keychain
  # access need none under the hardened runtime.
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" \
    "$APP/Contents/MacOS/$APP_NAME"
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$APP"
fi
codesign --verify --deep --strict "$APP"

echo "Built $APP (version $VERSION)"
