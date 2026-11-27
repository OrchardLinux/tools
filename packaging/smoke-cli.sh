#!/bin/sh
# What a person does first with the command line, on this computer and
# without a device: install it as install.sh does, then let it look at the
# computer, fetch orchard's data and build a board's boot firmware.
#   packaging/smoke-cli.sh PROGRAM      PROGRAM: the built orchard
set -eu
cd "$(dirname "$0")/.."
program=$1
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

case "$(uname -s)" in
    Darwin) asset=orchard-macos ;;
    *) case "$(uname -m)" in
           x86_64|amd64) asset=orchard-linux-x86_64 ;;
           *) asset=orchard-linux-aarch64 ;;
       esac ;;
esac
# A directory stands in for the published build.
mkdir "$work/builds"
cp "$program" "$work/builds/$asset"
printf 'smoke\n' > "$work/builds/BUILD"
(cd "$work/builds" && { sha256sum "$asset" BUILD 2>/dev/null || shasum -a 256 "$asset" BUILD; } > SHA256SUMS)
ORCHARD_BUILDS_URL="file://$work/builds" ORCHARD_BIN="$work/bin" HOME="$work/home" sh install.sh
orchard="$work/bin/orchard"
[ -x "$orchard" ]

# A changed download is refused and nothing is installed over the program.
printf 'x' >> "$work/builds/$asset"
if ORCHARD_BUILDS_URL="file://$work/builds" ORCHARD_BIN="$work/bin" HOME="$work/home" sh install.sh >/dev/null 2>&1; then
    echo 'install.sh took a file that does not match its checksum' >&2
    exit 1
fi
cmp -s "$program" "$orchard"

export ORCHARD_CACHE="$work/cache"
"$orchard" --version
# doctor ends with a failure where something is missing; it has to list the checks either way.
"$orchard" doctor > "$work/doctor" || true
cat "$work/doctor"
grep -q 'Check' "$work/doctor"
"$orchard" data update
"$orchard" boards > "$work/boards"
grep -q 'j71b' "$work/boards"
"$orchard" fw build p105 --out "$work/firmware"
[ -s "$work/firmware/ibss.raw" ]
echo "The command line installs and works without a device."
