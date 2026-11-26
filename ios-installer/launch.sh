#!/bin/bash
# Starts the in-app installer on the connected iPhone or iPad through Dopamine
# (no DFU mode). Builds it first when it is not built yet.
#
#   ./launch.sh [--jailbroken] [--allow-unsupported] [--index URL]
set -euo pipefail
cd "$(dirname "$0")"
[ -x out/device/orchard-installer-helper ] || ./build.sh
exec python3 launch.py "$@"
