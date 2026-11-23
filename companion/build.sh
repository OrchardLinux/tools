#!/bin/bash
# Builds the companion for iOS 12+ (arm64) into out/: the app bundle, the
# root helper and the entry program, ad-hoc signed. Needs Xcode's iOS SDK and
# ldid. The helper links resize-apfs's engine from a checkout at the pinned
# commit (RESIZE_APFS, default ../../resize-apfs next to orchard2).
set -euo pipefail
cd "$(dirname "$0")"

# --simulator: only the app, for the iOS simulator, to check the screens.
if [ "${1:-}" = "--simulator" ]; then
  app=$PWD/out-simulator/OrchardCompanion.app
  rm -rf "$app" && mkdir -p "$app/Fonts" "$app/Icons"
  xcrun --sdk iphonesimulator swiftc -target arm64-apple-ios15.0-simulator -sdk "$(xcrun --sdk iphonesimulator --show-sdk-path)" \
    -swift-version 5 -O -parse-as-library App/App.swift -o "$app/OrchardCompanion"
  cp App/Info.plist "$app/" && plutil -replace CFBundleSupportedPlatforms -json '["iPhoneSimulator"]' "$app/Info.plist"
  cp ../crates/app/assets/fonts/*.ttf "$app/Fonts/" && cp App/Icons/*.png "$app/Icons/"
  codesign --force --sign - --timestamp=none "$app"
  echo "Built $app"
  exit 0
fi
RESIZE_APFS_COMMIT=d1301cda3dc402fe3fda037b2c460f275f90ca94
resize=${RESIZE_APFS:-$PWD/../../resize-apfs}
actual=$(git -C "$resize" rev-parse HEAD)
[ "$actual" = "$RESIZE_APFS_COMMIT" ] || { echo "resize-apfs is at $actual, not the pinned $RESIZE_APFS_COMMIT" >&2; exit 1; }
[ -z "$(git -C "$resize" status --porcelain -- Sources)" ] || { echo "resize-apfs has local changes in Sources/" >&2; exit 1; }
command -v ldid >/dev/null || { echo 'ldid is required for ad-hoc signing' >&2; exit 1; }

sdk=$(xcrun --sdk iphoneos --show-sdk-path)
# iOS 12: the compiler then also rejects any newer API (resize-apfs builds the same way).
target=arm64-apple-ios12.0
out=$PWD/out
work=$PWD/.build
rm -rf "$out" "$work"
mkdir -p "$out" "$work"
swiftc() { xcrun --sdk iphoneos swiftc -target "$target" -sdk "$sdk" -swift-version 5 -O "$@"; }
clang() { xcrun --sdk iphoneos clang -target "$target" -isysroot "$sdk" -Os "$@"; }

# resize-apfs's engine: ResizeCore, NativeSystem and ResizeDevice.
clang -I "$resize/Sources/NativeSystem/include" -c "$resize/Sources/NativeSystem/NativeSystem.c" -o "$work/NativeSystem.o"
swiftc -parse-as-library -emit-library -static -emit-module -module-name ResizeCore \
  -emit-module-path "$work/ResizeCore.swiftmodule" "$resize"/Sources/ResizeCore/*.swift -o "$work/libResizeCore.a"
swiftc -parse-as-library -emit-library -static -emit-module -module-name ResizeDevice \
  -emit-module-path "$work/ResizeDevice.swiftmodule" -I "$work" -I "$resize/Sources/NativeSystem/include" \
  "$resize"/Sources/ResizeDevice/*.swift -o "$work/libResizeDevice.a"

# The helper.
clang -c Helper/native.c -o "$work/native.o"
swiftc -I "$work" -I "$resize/Sources/NativeSystem/include" -L "$work" -lResizeDevice -lResizeCore \
  "$work/NativeSystem.o" "$work/native.o" -framework IOKit -framework CoreFoundation -framework MobileCoreServices \
  Helper/main.swift -o "$out/orchard-companion"

# The entry program.
clang Entry/entry.c -o "$out/orchard-companion-entry"

# The app.
app="$out/OrchardCompanion.app"
mkdir -p "$app/Fonts" "$app/Icons"
swiftc -parse-as-library App/App.swift -Xlinker -rpath -Xlinker /usr/lib/swift -Xlinker -rpath -Xlinker @executable_path/Frameworks -o "$app/OrchardCompanion"
cp App/Info.plist "$app/Info.plist"
plutil -convert binary1 "$app/Info.plist"
cp ../crates/app/assets/fonts/*.ttf "$app/Fonts/"
cp App/Icons/*.png "$app/Icons/"
# The home-screen icon (loose files, as without an asset catalog).
cp App/AppIcon/*.png "$app/"
xcrun ibtool --minimum-deployment-target 12.0 --target-device iphone --target-device ipad \
  --compile "$app/LaunchScreen.storyboardc" App/Launch/LaunchScreen.storyboard

# Swift runtime: iOS 12.2 and later have it in /usr/lib/swift, searched first.
# iOS 12.0 and 12.1 don't; the checkra1n route's payload will bring Apple's
# back-deployment runtime in the app's Frameworks (as resize-apfs's
# scripts/build-device-ios12.sh does). Not here: palera1n needs iOS 15, and the
# runtime would add about 11 MB to the installer and every overlay.

# No debug maps or build paths in what ships, then ad-hoc signatures.
for binary in "$app/OrchardCompanion" "$out/orchard-companion" "$out/orchard-companion-entry"; do
  xcrun --sdk iphoneos strip -S "$binary"
done
codesign --force --sign - --timestamp=none --generate-entitlement-der --entitlements App/app.entitlements.plist "$app"
codesign --verify --deep --strict "$app"
ldid -SHelper/helper.entitlements.plist "$out/orchard-companion"
ldid -SHelper/helper.entitlements.plist "$out/orchard-companion-entry"
echo "Built the companion in $out"
