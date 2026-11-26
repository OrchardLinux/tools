#!/bin/bash
# Builds only the app, for the iOS simulator, into out-simulator/ to check the
# screens: `xcrun simctl install booted out-simulator/OrchardInstaller.app`,
# then `xcrun simctl launch booted dev.orchard.installer --mock I1.2`.
# First the sources are compiled for arm64 iOS 12.0 as the device build does
# (nothing is kept), so an API newer than iOS 12 fails here too.
set -euo pipefail
cd "$(dirname "$0")"

xcrun --sdk iphoneos swiftc -target arm64-apple-ios12.0 -sdk "$(xcrun --sdk iphoneos --show-sdk-path)" \
  -swift-version 5 -parse-as-library -typecheck App/*.swift

app=$PWD/out-simulator/OrchardInstaller.app
rm -rf "$app" && mkdir -p "$app/Fonts"
xcrun --sdk iphonesimulator swiftc -target arm64-apple-ios15.0-simulator -sdk "$(xcrun --sdk iphonesimulator --show-sdk-path)" \
  -swift-version 5 -O -parse-as-library App/*.swift -o "$app/OrchardInstaller"
cp App/Info.plist "$app/" && plutil -replace CFBundleSupportedPlatforms -json '["iPhoneSimulator"]' "$app/Info.plist"
cp ../crates/app/assets/fonts/*.ttf "$app/Fonts/"
# The wordmark of what an unstamped installer installs.
cp ../packaging/omarchy-a-wordmark.svg "$app/wordmark.svg"
cp App/AppIcon/*.png "$app/"
xcrun ibtool --minimum-deployment-target 12.0 --target-device iphone --target-device ipad \
  --compile "$app/LaunchScreen.storyboardc" App/Launch/LaunchScreen.storyboard >/dev/null
codesign --force --sign - --timestamp=none "$app"
echo "Built $app"
