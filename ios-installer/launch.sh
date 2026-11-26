#!/bin/bash
# Puts the in-app installer on the connected iPhone or iPad (no DFU mode): one
# app, the installer inside Dopamine. Builds it first when it is not built yet.
#
#   ./launch.sh [--allow-unsupported] [--index URL]
set -euo pipefail
cd "$(dirname "$0")"
[ -f out/device/access/OrchardInstaller.dylib ] || ./build.sh
exec python3 launch.py "$@"
