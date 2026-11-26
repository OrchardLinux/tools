#!/bin/bash
# Builds the in-app installer for iOS 12.2 and later (arm64) into out/device:
# the app, its root helper and the e2fsprogs programs the helper runs, all
# ad-hoc signed. Needs Xcode's iOS SDK, the aarch64-apple-ios Rust target and
# ldid. The helper links resize-apfs's engine from a checkout at the pinned
# commit (RESIZE_APFS, default ../../resize-apfs-engine next to this checkout)
# and the installer's own engine (crates/inapp).
set -euo pipefail
cd "$(dirname "$0")"
RESIZE_APFS_COMMIT=19d14d11ef0c70f1dc135de62ff306efc9389e04
root=$PWD
resize=${RESIZE_APFS:-$root/../../resize-apfs-engine}
actual=$(git -C "$resize" rev-parse HEAD)
[ "$actual" = "$RESIZE_APFS_COMMIT" ] || { echo "resize-apfs is at $actual, not the pinned $RESIZE_APFS_COMMIT" >&2; exit 1; }
[ -z "$(git -C "$resize" status --porcelain -- Sources)" ] || { echo "resize-apfs has local changes in Sources/" >&2; exit 1; }
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

# The app.
app="$out/OrchardInstaller.app"
mkdir -p "$app/Fonts"
swiftc -parse-as-library App/*.swift -Xlinker -rpath -Xlinker /usr/lib/swift -o "$app/OrchardInstaller"
cp App/Info.plist "$app/Info.plist"
plutil -convert binary1 "$app/Info.plist"
cp ../crates/app/assets/fonts/*.ttf "$app/Fonts/"
# The wordmark of what an unstamped installer installs.
cp ../packaging/omarchy-a-wordmark.svg "$app/wordmark.svg"
# The home-screen icon (loose files, as without an asset catalog).
cp App/AppIcon/*.png "$app/"
xcrun ibtool --minimum-deployment-target 12.0 --target-device iphone --target-device ipad \
  --compile "$app/LaunchScreen.storyboardc" App/Launch/LaunchScreen.storyboard

# No debug maps or build paths in what ships, then ad-hoc signatures.
for binary in "$app/OrchardInstaller" "$out/orchard-installer-helper"; do
  xcrun --sdk iphoneos strip -S "$binary"
  minos=$(xcrun vtool -show-build "$binary" | awk '/minos/ {print $2}')
  [ "$minos" = 12.0 ] || { echo "$binary is built for iOS $minos, not 12.0" >&2; exit 1; }
done
codesign --force --sign - --timestamp=none --generate-entitlement-der --entitlements App/app.entitlements.plist "$app"
codesign --verify --deep --strict "$app"
ldid -SHelper/helper.entitlements.plist "$out/orchard-installer-helper"
echo "Built the installer in $out"
