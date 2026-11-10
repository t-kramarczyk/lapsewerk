#!/bin/sh
# Builds Lapsewerk.app and Lapsewerk-<version>.dmg in target/macos.
#   packaging/macos/build-app.sh                 ad-hoc signed (this Mac only)
#   SIGN_ID="Developer ID Application: …" packaging/macos/build-app.sh
#   … plus NOTARY_KEY_ID, NOTARY_ISSUER, NOTARY_KEY_FILE (.p8): notarized and stapled
set -eu
cd "$(dirname "$0")/../.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
SIGN_ID=${SIGN_ID:--}
OUT=target/macos
APP=$OUT/Lapsewerk.app

# Built apart (target/bundle), without the build machine's paths inside (scripts/build-release.sh)
# and without the helper's build path: the bundle carries it beside the programs.
BUILD=${CARGO_TARGET_DIR:-target/bundle}
CARGO_TARGET_DIR=$BUILD LAPSEWERK_BUNDLE=1 scripts/build-release.sh
HELPER=$(ls -t "$BUILD"/release/build/lapsewerk-video-*/out/lapsewerk-encode | head -1)
[ -f "$HELPER" ] || { echo "encoder helper not found" >&2; exit 1; }

rm -rf "${APP:?}" "$OUT/icon.iconset"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$OUT/icon.iconset"
cp "$BUILD/release/lapsewerk-app" "$BUILD/release/lapsewerk" "$HELPER" "$APP/Contents/MacOS/"
cp LICENSE NOTICE THIRD-PARTY-LICENSES.txt "$APP/Contents/Resources/"

for s in 16 32 128 256 512; do
  rsvg-convert -w $s -h $s packaging/macos/AppIcon.svg -o "$OUT/icon.iconset/icon_${s}x${s}.png"
  rsvg-convert -w $((s * 2)) -h $((s * 2)) packaging/macos/AppIcon.svg -o "$OUT/icon.iconset/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$OUT/icon.iconset" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "${OUT:?}/icon.iconset"

sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null

sign() {
  if [ "$SIGN_ID" = "-" ]; then
    codesign --force --sign - "$@"
  else
    codesign --force --options runtime --timestamp --sign "$SIGN_ID" "$@"
  fi
}
for b in lapsewerk-encode lapsewerk lapsewerk-app; do
  sign --identifier "de.tk-craft.lapsewerk.${b}" "$APP/Contents/MacOS/$b"
done
sign "$APP"
codesign --verify --strict --deep "$APP"

DMG=$OUT/Lapsewerk-$VERSION.dmg
STAGE=$OUT/dmg
rm -rf "${STAGE:?}" "$DMG"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Programme"
hdiutil create -quiet -volname "Lapsewerk $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO "$DMG"
rm -rf "${STAGE:?}"
[ "$SIGN_ID" = "-" ] || codesign --force --timestamp --sign "$SIGN_ID" "$DMG"

# Nothing of the build machine inside (home folder, user name).
if grep -l -a "$HOME" "$APP/Contents/MacOS/"*; then echo "build paths left in the programs" >&2; exit 1; fi

if [ -n "${NOTARY_KEY_ID:-}" ]; then
  xcrun notarytool submit "$DMG" --key "$NOTARY_KEY_FILE" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" --wait
  xcrun stapler staple "$DMG"
  xcrun stapler staple "$APP"
  spctl --assess --type open --context context:primary-signature -v "$DMG"
  spctl --assess --type execute -v "$APP"
fi
shasum -a 256 "$DMG" | tee "$DMG.sha256"
echo "built: $APP, $DMG"
