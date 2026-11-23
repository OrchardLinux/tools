#!/bin/bash
# Linux generic artifacts (out/generic), after packaging/build-linux.sh <arch>:
#   orchard-installer-linux-<arch>  the installer, unstamped: a distro stamps
#                                   it (packaging/stamp.py)
#   Orchard-linux-<arch>.tar.gz     the neutral launcher: orchard, its desktop
#                                   entry and icons (the installer puts them in place)
#   packaging/package-linux.sh aarch64|x86_64
set -euo pipefail
cd "$(dirname "$0")/.."
arch=$1 out=out/generic
stage=$out/stage-linux-$arch
rm -rf "$stage" && mkdir -p "$stage/applications" "$stage/icons"
cp "out/linux/orchard-installer-$arch" "$out/orchard-installer-linux-$arch"
cp "out/linux/orchard-$arch" "$stage/orchard"
cp -R packaging/icons/hicolor "$stage/icons/"
cat > "$stage/applications/orchard.desktop" <<D
[Desktop Entry]
Type=Application
Name=Orchard
Comment=Start the system installed on your iPad or iPhone
Exec=orchard
Icon=orchard
Categories=System;
Terminal=false
D
tar -C "$stage" -czf "$out/Orchard-linux-$arch.tar.gz" .
rm -rf "$stage"
sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1 > "$out/VERSION"
ls -lh "$out/orchard-installer-linux-$arch" "$out/Orchard-linux-$arch.tar.gz"
