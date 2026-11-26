#!/bin/bash
# Builds the in-app installer (arm64) into out/device/access: what
# resize-apfs's scripts/embed.py puts into the Dopamine app, so that one app
# is installed without DFU. That is the installer's interface as a library,
# its resources, and its root helper with the e2fsprogs programs it runs as
# payloads. Needs Xcode's iOS SDK, the aarch64-apple-ios Rust target and ldid.
#
# From a resize-apfs checkout at the pinned commit (RESIZE_APFS, default
# ../../resize-apfs-engine next to this checkout): the resize engine the
# helper links, and the Access folder (the code that gets storage access
# through Dopamine and starts the helper). The installer's own engine is
# crates/inapp.
set -euo pipefail
cd "$(dirname "$0")"
RESIZE_APFS_COMMIT=98d1c2fb646eb2c9c5a19b8818ce2101e3edf55d
root=$PWD
resize=${RESIZE_APFS:-$root/../../resize-apfs-engine}
actual=$(git -C "$resize" rev-parse HEAD)
[ "$actual" = "$RESIZE_APFS_COMMIT" ] || { echo "resize-apfs is at $actual, not the pinned $RESIZE_APFS_COMMIT" >&2; exit 1; }
[ -z "$(git -C "$resize" status --porcelain -- Sources Access)" ] || { echo "resize-apfs has local changes in Sources/ or Access/" >&2; exit 1; }
command -v ldid >/dev/null || { echo 'ldid is required for ad-hoc signing' >&2; exit 1; }

sdk=$(xcrun --sdk iphoneos --show-sdk-path)
# iOS 12: the compiler then also rejects any newer API.
target=arm64-apple-ios12.0
out=$root/out/device
work=$root/.build/device
rm -rf "$out" "$work"
mkdir -p "$out" "$work"
swiftc() { xcrun --sdk iphoneos swiftc -target "$target" -sdk "$sdk" -swift-version 5 -O "$@"; }
clang() { xcrun --sdk iphoneos clang -target "$target" -isysroot "$sdk" -Os "$@"; }

# The installer's engine.
(cd "$root/.." && IPHONEOS_DEPLOYMENT_TARGET=12.0 cargo build --release -p orchard-inapp --target aarch64-apple-ios)
engine=$root/../target/aarch64-apple-ios/release/liborchard_inapp.a

# resize-apfs's engine: ResizeCore, NativeSystem and ResizeDevice.
clang -I "$resize/Sources/NativeSystem/include" -c "$resize/Sources/NativeSystem/NativeSystem.c" -o "$work/NativeSystem.o"
swiftc -parse-as-library -emit-library -static -emit-module -module-name ResizeCore \
  -emit-module-path "$work/ResizeCore.swiftmodule" "$resize"/Sources/ResizeCore/*.swift -o "$work/libResizeCore.a"
swiftc -parse-as-library -emit-library -static -emit-module -module-name ResizeDevice \
  -emit-module-path "$work/ResizeDevice.swiftmodule" -I "$work" -I "$resize/Sources/NativeSystem/include" \
  "$resize"/Sources/ResizeDevice/*.swift -o "$work/libResizeDevice.a"

# The helper. iOS 12.2 and later have the Swift runtime in /usr/lib/swift.
swiftc -I "$work" -I "$resize/Sources/NativeSystem/include" -I Helper -L "$work" -lResizeDevice -lResizeCore \
  "$work/NativeSystem.o" "$engine" -framework IOKit -framework CoreFoundation -framework MobileCoreServices \
  -framework Security -lresolv -liconv Helper/main.swift -o "$out/orchard-installer-helper"

# e2fsprogs, for the helper to change the new root with.
[ -x out/e2fsprogs-ios/debugfs ] || ./build-e2fsprogs.sh ios
mkdir -p "$out/e2fsprogs"
cp out/e2fsprogs-ios/{tune2fs,resize2fs,debugfs,e2fsck} "$out/e2fsprogs/"
cp out/e2fsprogs-ios/NOTICE "$out/e2fsprogs/NOTICE"

# The app, as the library the Dopamine app loads (iOS 15 and later, as
# Dopamine). AppDelegate.swift is the app's start as a program of its own,
# which only the simulator build uses.
access="$out/access"
mkdir -p "$access/Resources/Fonts"
embedded=arm64-apple-ios15.0
xcrun --sdk iphoneos clang -target "$embedded" -isysroot "$sdk" -Os -fobjc-arc -fmodules \
  -c "$resize/Access/AccessBridge.m" -o "$work/AccessBridge.o"
sources=("$resize/Access/StorageAccess.swift")
for file in App/*.swift; do [ "$file" = App/AppDelegate.swift ] || sources+=("$file"); done
xcrun --sdk iphoneos swiftc -target "$embedded" -sdk "$sdk" -swift-version 5 -O \
  -parse-as-library -emit-library -module-name OrchardInstaller "${sources[@]}" "$work/AccessBridge.o" \
  -Xlinker -install_name -Xlinker @executable_path/Frameworks/OrchardInstaller.dylib \
  -o "$access/OrchardInstaller.dylib"
cp App/Info.plist "$access/Info.plist"
cp ../crates/app/assets/fonts/*.ttf "$access/Resources/Fonts/"
# The wordmark of what an unstamped installer installs.
cp ../packaging/omarchy-a-wordmark.svg "$access/Resources/wordmark.svg"
# The home-screen icon (loose files, as without an asset catalog).
cp App/AppIcon/*.png "$access/Resources/"
xcrun ibtool --minimum-deployment-target 12.0 --target-device iphone --target-device ipad \
  --compile "$access/Resources/LaunchScreen.storyboardc" App/Launch/LaunchScreen.storyboard

# No debug maps or build paths in what ships, then ad-hoc signatures.
xcrun --sdk iphoneos strip -x "$access/OrchardInstaller.dylib"
codesign --force --sign - --timestamp=none "$access/OrchardInstaller.dylib"
xcrun --sdk iphoneos strip -S "$out/orchard-installer-helper"
minos=$(xcrun vtool -show-build "$out/orchard-installer-helper" | awk '/minos/ {print $2}')
[ "$minos" = 12.0 ] || { echo "The helper is built for iOS $minos, not 12.0" >&2; exit 1; }
ldid -SHelper/helper.entitlements.plist "$out/orchard-installer-helper"

# The helper and its programs travel in the app with a line in front of them:
# signing the app with an Apple ID re-signs every program it finds, which
# would cost them their own signatures (StorageAccess takes the line off).
for program in "$out/orchard-installer-helper" "$out"/e2fsprogs/{tune2fs,resize2fs,debugfs,e2fsck}; do
  { printf 'RESIZE-APFS-HELPER\n'; cat "$program"; } > "$access/Resources/$(basename "$program").payload"
done
echo "Built the installer in $access"
