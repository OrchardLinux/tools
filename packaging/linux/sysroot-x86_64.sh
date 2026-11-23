#!/bin/bash
# The x86_64 libraries the cross build links against, from an x86_64 Arch image
# (pacman runs fine under the podman machine's qemu; rustc doesn't):
# out/linux/sysroot-x86_64. Run once before packaging/build-linux.sh x86_64.
set -euo pipefail
cd "$(dirname "$0")/../.."
rm -rf out/linux/sysroot-x86_64 && mkdir -p out/linux
podman run --rm --platform linux/amd64 -v "$PWD/out/linux:/out" docker.io/library/archlinux:base-devel sh -c '
sed -i "/^\[options\]/a DisableSandbox" /etc/pacman.conf
pacman -Sy --noconfirm --needed libxcb libxkbcommon libxkbcommon-x11 >/dev/null
mkdir -p /out/sysroot-x86_64
cd / && tar -cf - usr/include usr/lib/gcc usr/lib/crt1.o usr/lib/crti.o usr/lib/crtn.o usr/lib/Scrt1.o \
  $(ls -d usr/lib/libc.* usr/lib/libc_nonshared.a usr/lib/libm.* usr/lib/libm-* usr/lib/libmvec* usr/lib/libgcc_s* \
         usr/lib/libpthread* usr/lib/libdl* usr/lib/librt* usr/lib/libutil* usr/lib/ld-linux-x86-64* \
         usr/lib/libxcb* usr/lib/libxkbcommon* usr/lib/libXau* usr/lib/libXdmcp* usr/lib/libxml2* \
         usr/lib/pkgconfig usr/share/pkgconfig 2>/dev/null) lib64 2>/dev/null | tar -C /out/sysroot-x86_64 -xf - || true'
echo "Extracted out/linux/sysroot-x86_64"
