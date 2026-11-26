#!/bin/bash
# Test bundles (not the release packaging): "Install Omarchy A.app" and
# "Orchard.app" in target/final, the current debug builds started live with a
# local release and a test Pongo, logging to $LOG_DIR.
#   packaging/test-bundles.sh RELEASE_DIR PONGO_BIN LOG_DIR
set -euo pipefail
cd "$(dirname "$0")/.."
release=$1 pongo=$2 logs=$3
[[ -x target/debug/orchard-installer && -x target/debug/orchard ]] || { echo "build first: cargo build -p orchard-installer-app" >&2; exit 1; }
wordmark=$PWD/packaging/omarchy-a-wordmark.svg
make() {  # name, bundle id, binary, extra flags, log name
  local app="target/final/$1.app/Contents"
  rm -rf "target/final/$1.app"
  mkdir -p "$app/MacOS" "$app/Resources"
  cp "target/debug/$3" "$app/MacOS/$3"
  cp packaging/icons/Orchard.icns "$app/Resources/Orchard.icns"
  cat > "$app/MacOS/launch" <<SH
#!/bin/sh
exec "\$(dirname "\$0")/$3" $4 --live --local "$release" --pongo "$pongo" >> "$logs/$5" 2>&1
SH
  chmod +x "$app/MacOS/launch"
  cat > "$app/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>launch</string>
<key>CFBundleIdentifier</key><string>$2</string>
<key>CFBundleName</key><string>$1</string>
<key>CFBundleDisplayName</key><string>$1</string>
<key>CFBundleIconFile</key><string>Orchard</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.0.1</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PL
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$PWD/target/final/$1.app"
}
make "Install Omarchy A" org.orchardlinux.install-omarchy-a orchard-installer "--name 'Omarchy A' --wordmark '$wordmark'" live.log
make "Orchard" org.orchardlinux.launcher orchard "" launcher.log
echo "Built target/final/Install Omarchy A.app and target/final/Orchard.app"
