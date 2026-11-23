#!/bin/bash
# Linux release binaries under podman (orchard's way): out/linux/<bin>-<arch>
# for orchard-installer and orchard (the launcher).
#   packaging/build-linux.sh aarch64|x86_64
# The source is mounted read-only; each arch builds into its own volume.
set -euo pipefail
cd "$(dirname "$0")/.."
arch=$1
case $arch in
  aarch64) platform=linux/arm64; base=localhost/omarchy-a-arch-builder:latest ;;
  # x86_64: cross-compiled in the arm64 container (rustc crashes under the
  # podman machine's qemu), linked against x86_64 Arch libraries extracted to
  # out/linux/sysroot-x86_64 (packaging/linux/sysroot-x86_64.sh).
  x86_64)  platform=linux/arm64; base=localhost/omarchy-a-arch-builder:latest; cross=1 ;;
  *) echo "aarch64 or x86_64" >&2; exit 1 ;;
esac
image=orchard2-linux-builder:aarch64
# Built once; rebuilt only when missing (Arch ARM mirrors can stall).
podman image exists "$image" || podman build --platform "$platform" --build-arg BASE="$base" -t "$image" packaging/linux
mkdir -p out/linux
podman run --rm --platform "$platform" \
  -v "$PWD:/src:ro" -v "orchard2-target-$arch:/target" -v "orchard2-cargo-$arch:/root/.cargo/registry" \
  -v "$PWD/out/linux:/out" -e CARGO_TARGET_DIR=/target -e CARGO_INCREMENTAL=0 "$image" \
  sh -c "$(if [[ ${cross:-} == 1 ]]; then cat <<'SH'
set -e
rustup target add x86_64-unknown-linux-gnu >/dev/null
sys=/out/sysroot-x86_64
# The linker: rust-lld from the toolchain (as ld.lld), no package needed.
lld=$(rustc --print sysroot)/lib/rustlib/aarch64-unknown-linux-gnu/bin/gcc-ld
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=clang
export RUSTFLAGS="-C link-arg=--target=x86_64-unknown-linux-gnu -C link-arg=--sysroot=$sys -C link-arg=-fuse-ld=lld -C link-arg=-B$lld"
export CC_x86_64_unknown_linux_gnu="clang --target=x86_64-unknown-linux-gnu --sysroot=$sys -fuse-ld=lld -B$lld"
export CXX_x86_64_unknown_linux_gnu="clang++ --target=x86_64-unknown-linux-gnu --sysroot=$sys -fuse-ld=lld -B$lld"
export AR_x86_64_unknown_linux_gnu=ar
export PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG_SYSROOT_DIR=$sys PKG_CONFIG_LIBDIR=$sys/usr/lib/pkgconfig:$sys/usr/share/pkgconfig
cargo build --release -p orchard-installer-app --target x86_64-unknown-linux-gnu
for b in orchard-installer orchard; do cp /target/x86_64-unknown-linux-gnu/release/$b /out/$b-x86_64; done
SH
else echo "cargo build --release -p orchard-installer-app && for b in orchard-installer orchard; do cp /target/release/\$b /out/\$b-$arch; done"; fi)"
echo "Built out/linux/orchard-installer-$arch and out/linux/orchard-$arch"
