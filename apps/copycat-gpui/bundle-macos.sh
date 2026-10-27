#!/bin/sh
# Assemble Copycat.app from universal binaries, sign it, and make the .dmg.
# usage: bundle-macos.sh <version> <identity> <gui-binary> <daemon-binary> <out-dir>
# <identity> is the codesign identity ("-" for ad hoc). Both binaries are the
# lipo'd universal builds. Same bundle id as the Tauri app, so an Accessibility
# grant made to it carries over (the signing certificate is the same too).
set -eu
VERSION=$1 IDENTITY=$2 GUI=$3 DAEMON=$4 OUT=$5
HERE=$(cd "$(dirname "$0")" && pwd)
APP="$OUT/Copycat.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$GUI" "$APP/Contents/MacOS/copycat-gpui"
cp "$DAEMON" "$APP/Contents/MacOS/copycatd"   # found beside the executable
cp "$HERE/assets/icon.icns" "$APP/Contents/Resources/icon.icns"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Copycat</string>
  <key>CFBundleDisplayName</key><string>Copycat</string>
  <key>CFBundleIdentifier</key><string>dev.copycat.desktop</string>
  <key>CFBundleExecutable</key><string>copycat-gpui</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>10.15</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
# Inside out: the daemon first, then the bundle.
codesign --force --sign "$IDENTITY" "$APP/Contents/MacOS/copycatd"
codesign --force --sign "$IDENTITY" "$APP"
codesign --verify --deep --strict "$APP"
rm -f "$OUT/Copycat.dmg"
hdiutil create -volname Copycat -srcfolder "$APP" -ov -format UDZO "$OUT/Copycat-$VERSION.dmg"
