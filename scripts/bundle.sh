#!/usr/bin/env bash
# Build MacDirStat.app in target/release (or the directory given as $1).
#
# Signing: uses $MACDIRSTAT_SIGN_IDENTITY, else the first "Developer ID
# Application" identity in the keychain, else an ad-hoc signature.
#
# MACDIRSTAT_UNIVERSAL=1 builds for arm64 and x86_64 and merges them with lipo.
# MACDIRSTAT_DMG=1 also packages MacDirStat.dmg next to the app.
#
# Notarization (Developer ID only): set MACDIRSTAT_NOTARIZE=1 after storing
# credentials once with
#   xcrun notarytool store-credentials macdirstat-notary \
#     --apple-id <apple id> --team-id <team id>
# ($MACDIRSTAT_NOTARY_PROFILE overrides the profile name.)
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT_DIR=${1:-target/release}
APP="$OUT_DIR/MacDirStat.app"
BIN="$APP/Contents/MacOS/MacDirStat"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
if [[ "${MACDIRSTAT_UNIVERSAL:-0}" == 1 ]]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin
  cargo build --release --target aarch64-apple-darwin
  cargo build --release --target x86_64-apple-darwin
  lipo -create -output "$BIN" \
    target/aarch64-apple-darwin/release/macdirstat target/x86_64-apple-darwin/release/macdirstat
else
  cargo build --release
  cp target/release/macdirstat "$BIN"
fi

ICONSET=$(mktemp -d)/MacDirStat.iconset
mkdir -p "$ICONSET"
"$BIN" --export-icon "$ICONSET/icon_256x256.png"
for size in 16 32 128 256; do
  sips -z $size $size "$ICONSET/icon_256x256.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z $double $double "$ICONSET/icon_256x256.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/MacDirStat.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>MacDirStat</string>
  <key>CFBundleDisplayName</key><string>MacDirStat</string>
  <key>CFBundleIdentifier</key><string>io.github.andrewcolumbus.macdirstat</string>
  <key>CFBundleExecutable</key><string>MacDirStat</string>
  <key>CFBundleIconFile</key><string>MacDirStat</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
</dict>
</plist>
PLIST

# A stable Developer ID signature keeps the Full Disk Access grant across
# rebuilds; macOS ties the grant to the code signature.
IDENTITY=${MACDIRSTAT_SIGN_IDENTITY:-$(security find-identity -v -p codesigning 2>/dev/null |
  sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p' | head -1 || true)}
if [[ -n "$IDENTITY" ]]; then
  codesign --force --options runtime --timestamp --sign "$IDENTITY" "$APP"
  codesign --verify --strict "$APP"
  echo "Signed with $IDENTITY"
else
  codesign --force --sign - "$APP"
  echo "No Developer ID identity found; signed ad hoc"
fi

NOTARIZE=${MACDIRSTAT_NOTARIZE:-0}
if [[ "$NOTARIZE" == 1 && -z "$IDENTITY" ]]; then
  echo "Notarization needs a Developer ID signature" >&2
  exit 1
fi
NOTARY_AUTH=(--keychain-profile "${MACDIRSTAT_NOTARY_PROFILE:-macdirstat-notary}")

# Submits $1 and prints the notary log if Apple rejects it.
notarize() {
  local result id status
  result=$(xcrun notarytool submit "$1" "${NOTARY_AUTH[@]}" --wait --output-format json) || true
  id=$(plutil -extract id raw - <<<"$result" 2>/dev/null || true)
  status=$(plutil -extract status raw - <<<"$result" 2>/dev/null || true)
  echo "Notarization of $(basename "$1"): ${status:-failed} ${id}"
  if [[ "$status" == Accepted ]]; then
    return
  elif [[ -n "$id" ]]; then
    xcrun notarytool log "$id" "${NOTARY_AUTH[@]}" >&2
  else
    echo "$result" >&2
  fi
  exit 1
}

if [[ "$NOTARIZE" == 1 ]]; then
  ZIP="$OUT_DIR/MacDirStat-$VERSION.zip"
  ditto -c -k --keepParent "$APP" "$ZIP"
  notarize "$ZIP"
  xcrun stapler staple "$APP"
  rm "$ZIP"
  ditto -c -k --keepParent "$APP" "$ZIP"
  spctl --assess --type execute --verbose "$APP"
  echo "Notarized $ZIP"
fi

if [[ "${MACDIRSTAT_DMG:-0}" == 1 ]]; then
  DMG="$OUT_DIR/MacDirStat.dmg"
  STAGING=$(mktemp -d)
  ditto "$APP" "$STAGING/MacDirStat.app"
  ln -s /Applications "$STAGING/Applications"
  rm -f "$DMG"
  hdiutil create -volname "MacDirStat $VERSION" -srcfolder "$STAGING" -format UDZO -quiet "$DMG"
  rm -rf "$STAGING"
  if [[ -n "$IDENTITY" ]]; then
    codesign --force --timestamp --sign "$IDENTITY" "$DMG"
  fi
  if [[ "$NOTARIZE" == 1 ]]; then
    notarize "$DMG"
    xcrun stapler staple "$DMG"
    spctl --assess --type open --context context:primary-signature --verbose "$DMG"
  fi
  echo "Built $DMG"
fi
echo "Built $APP"
