#!/usr/bin/env bash
set -euo pipefail
PORT="${PORT:-9008}"
adb shell "pkill -9 -f hsd" >/dev/null 2>&1 || true
adb forward --remove tcp:$PORT >/dev/null 2>&1 || true
echo "daemon stopped"
