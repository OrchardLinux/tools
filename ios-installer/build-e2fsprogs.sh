#!/bin/bash
# Builds the e2fsprogs programs the installer's helper runs on the new root
# (tune2fs, resize2fs, debugfs, e2fsck) into out/e2fsprogs-TARGET.
#
#   build-e2fsprogs.sh ios     arm64, iOS 12 and later, ad-hoc signed
#   build-e2fsprogs.sh host    this computer, for the tests
#
# iOS mounts no ext4, so the helper changes the installed root with these
# programs, as the install system does with its own on Linux. Upstream source,
# pinned by SHA-256, with one rename (et-link.patch: `link` is a function in
# Darwin's unistd.h) and a shim for what the iPhoneOS SDK lacks.
set -euo pipefail
cd "$(dirname "$0")"
root=$PWD
target=${1:?ios or host}
version=1.47.4
sha256=fd5bf388cbdbe006a3d3b318d983b2948382440acc85a87f1e7d108653e8db0b
work=$root/.build/e2fsprogs
mkdir -p "$work"
archive=$work/e2fsprogs-$version.tar.xz
if [ ! -f "$archive" ]; then
  curl -fL --retry 2 "https://www.kernel.org/pub/linux/kernel/people/tytso/e2fsprogs/v$version/e2fsprogs-$version.tar.xz" -o "$archive.partial"
  mv "$archive.partial" "$archive"
fi
printf '%s  %s\n' "$sha256" "$archive" | shasum -a 256 -c - >/dev/null
src=$work/e2fsprogs-$version
if [ ! -f "$src/.patched" ]; then
  rm -rf "$src"
  tar -xJf "$archive" -C "$work"
  patch -s -d "$src" -p1 < "$root/e2fsprogs/et-link.patch"
  touch "$src/.patched"
fi
build=$work/build-$target
out=$root/out/e2fsprogs-$target
rm -rf "$build" "$out"
mkdir -p "$build" "$out"
case $target in
  ios)
    sdk=$(xcrun --sdk iphoneos --show-sdk-path)
    cc="$(xcrun --sdk iphoneos --find clang) -target arm64-apple-ios12.0 -isysroot $sdk"
    cflags="-Os -include $root/e2fsprogs/ios-compat.h"
    host=--host=aarch64-apple-darwin ;;
  host)
    cc="$(xcrun --find clang) -isysroot $(xcrun --sdk macosx --show-sdk-path)"
    cflags=-Os
    host= ;;
  *) echo "unknown target $target" >&2; exit 2 ;;
esac
cd "$build"
# Private static libraries: nothing from the building computer is linked.
CC="$cc" CFLAGS="$cflags" LDFLAGS='-Wl,-dead_strip' PKG_CONFIG=/usr/bin/false \
  "$src/configure" $host --prefix=/usr --disable-nls --disable-elf-shlibs --disable-bsd-shlibs \
    --disable-fuse2fs --disable-defrag --disable-uuidd --disable-mmp --disable-tdb \
    --disable-backtrace --disable-fsck --disable-e2initrd-helper --without-libarchive \
    --enable-libuuid --enable-libblkid > configure.log 2>&1
make -j4 libs > build.log 2>&1
for spec in 'misc tune2fs' 'e2fsck e2fsck' 'debugfs debugfs' 'resize resize2fs'; do
  read -ra parts <<< "$spec"
  make -j4 -C "${parts[0]}" "${parts[@]:1}" >> build.log 2>&1
done
for spec in misc/tune2fs e2fsck/e2fsck debugfs/debugfs resize/resize2fs; do
  name=${spec##*/}
  cp "$build/$spec" "$out/$name"
  xcrun strip -S "$out/$name"
  if [ "$target" = ios ]; then
    ldid -S"$root/e2fsprogs/tool.entitlements.plist" "$out/$name"
  fi
  chmod 755 "$out/$name"
done
cp "$src/NOTICE" "$out/NOTICE"
echo "Built e2fsprogs $version for $target in $out"
