#!/bin/bash
# macOS generic artifacts (out/generic), universal (Apple Silicon + Intel),
# ad-hoc signed (no Developer ID; the install script downloads with curl, which
# sets no quarantine flag):
#   Orchard-Installer-macOS.zip  "Orchard Installer.app", unstamped: a distro
#                                stamps it (packaging/stamp.py)
#   Orchard-macOS.zip            "Orchard.app", the neutral launcher
# Orchard's own version (the workspace's), also in out/generic/VERSION.
#   packaging/build-macos.sh
set -euo pipefail
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
out=out/generic stage=out/generic/stage-macos
for bin in orchard-installer orchard; do
  for dir in target/release target/x86_64-apple-darwin/release; do
    [[ -x $dir/$bin ]] || { echo "build first: cargo build --release -p orchard-installer-app [--target x86_64-apple-darwin]" >&2; exit 1; }
  done
done
rm -rf "$stage" && mkdir -p "$stage" "$out"
bundle() {  # bundle name, executable, bundle id, zip
  local app="$stage/$1.app/Contents"
  mkdir -p "$app/MacOS" "$app/Resources"
  lipo -create "target/release/$2" "target/x86_64-apple-darwin/release/$2" -output "$app/MacOS/$2"
  cp packaging/icons/Orchard.icns "$app/Resources/Orchard.icns"
  cat > "$app/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>$2</string>
<key>CFBundleIdentifier</key><string>$3</string>
<key>CFBundleName</key><string>$1</string>
<key>CFBundleDisplayName</key><string>$1</string>
<key>CFBundleIconFile</key><string>Orchard</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>$version</string>
<key>LSMinimumSystemVersion</key><string>11.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PL
  codesign --force --sign - --timestamp=none "$stage/$1.app"
  rm -f "$out/$4"
  (cd "$stage" && ditto -c -k --sequesterRsrc --keepParent "$1.app" "../$4")
}
bundle "Orchard Installer" orchard-installer org.orchardlinux.installer Orchard-Installer-macOS.zip
bundle "Orchard" orchard-launcher org.orchardlinux.launcher Orchard-macOS.zip
rm -rf "$stage"
echo "$version" > "$out/VERSION"
ls -lh "$out"/*macOS.zip
