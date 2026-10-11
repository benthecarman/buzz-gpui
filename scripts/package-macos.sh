#!/usr/bin/env bash
# Builds an unsigned Buzz.app and wraps it in a .dmg. Runs on macOS, for the
# architecture of the machine it runs on. GStreamer comes from Homebrew and
# is not bundled: the app links against the Homebrew copy.
#
# Usage: scripts/package-macos.sh   -> dist/Buzz-<version>-macos-<arch>.dmg
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
cd "$here"

version="$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "buzz-gpui"))')"
arch="$(uname -m)"
cargo build --locked --release -p buzz-gpui

work="$(mktemp -d)"
app="$work/Buzz.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" dist
cp target/release/buzz-gpui "$app/Contents/MacOS/"

iconset="$work/buzz.iconset"
mkdir "$iconset"
for size in 128 256 512; do
  cp "assets/icons/buzz-$size.png" "$iconset/icon_${size}x${size}.png"
  if [ "$size" -lt 512 ]; then
    cp "assets/icons/buzz-$((size * 2)).png" "$iconset/icon_${size}x${size}@2x.png"
  fi
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/buzz.icns"

cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Buzz</string>
  <key>CFBundleDisplayName</key><string>Buzz</string>
  <key>CFBundleIdentifier</key><string>com.benthecarman.buzz-gpui</string>
  <key>CFBundleExecutable</key><string>buzz-gpui</string>
  <key>CFBundleIconFile</key><string>buzz</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Buzz records voice notes.</string>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>Buzz link</string>
      <key>CFBundleURLSchemes</key><array><string>buzz</string></array>
    </dict>
  </array>
</dict>
</plist>
EOF

# Ad-hoc signature: Apple Silicon refuses to run unsigned code at all.
codesign --force --deep --sign - "$app"

dmg="dist/Buzz-$version-macos-$arch.dmg"
rm -f "$dmg"
ln -s /Applications "$work/Applications"
rm -rf "$iconset"
hdiutil create -volname Buzz -srcfolder "$work" -ov -format UDZO "$dmg"
rm -rf "$work"
ls -la "$dmg"
