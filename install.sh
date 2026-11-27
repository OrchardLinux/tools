#!/bin/sh
# Install orchard2, the Orchard command line, or replace it with its newest
# build:
#   curl -fsSL https://raw.githubusercontent.com/OrchardLinux/tools/main/install.sh | sh
# It goes into ~/.local/bin (ORCHARD_BIN names another directory). Nothing
# needs root. `orchard2 update` does the same later.
set -eu
NAME=orchard2
BUILDS=${ORCHARD_BUILDS_URL:-https://github.com/OrchardLinux/tools/releases/download/nightly}
bin=${ORCHARD_BIN:-$HOME/.local/bin}

say() { printf '%s\n' "$*"; }
fail() { say "$*" >&2; exit 1; }

case "$(uname -s)" in
    Darwin) asset=$NAME-macos ;;
    Linux)
        case "$(uname -m)" in
            x86_64|amd64) asset=$NAME-linux-x86_64 ;;
            aarch64|arm64) asset=$NAME-linux-aarch64 ;;
            *) fail "No build for this computer ($(uname -m))." ;;
        esac ;;
    *) fail "No build for $(uname -s): macOS and Linux only." ;;
esac
command -v curl >/dev/null 2>&1 || fail 'curl is needed to download it.'
if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    fail 'sha256sum or shasum is needed to check the download.'
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/SHA256SUMS" "$BUILDS/SHA256SUMS" || fail "Couldn't download the build's checksums."
curl -fL --progress-bar -o "$work/$asset" "$BUILDS/$asset" || fail "Couldn't download $asset."
expected=$(awk -v name="$asset" '$2 == name || $2 == "./" name {print $1}' "$work/SHA256SUMS")
[ -n "$expected" ] || fail "The build lists no $asset."
[ "$(sha256 "$work/$asset")" = "$expected" ] || fail "$asset does not match the build's checksum; nothing was installed."

# Beside its place first, so that taking it is one rename, also while the
# old one runs.
mkdir -p "$bin"
chmod 755 "$work/$asset"
cp "$work/$asset" "$bin/.$NAME-$$"
mv -f "$bin/.$NAME-$$" "$bin/$NAME"
say "Installed $("$bin/$NAME" --version) in $bin."
case ":$PATH:" in
    *":$bin:"*) ;;
    *) say "$bin is not on your PATH: add it, or run $bin/$NAME." ;;
esac
