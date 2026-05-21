#!/usr/bin/env bash
# Throughput / latency benchmark for the handsets daemon.
# (uiautomator dump on Android 15 currently exits 137; left in for older devices.)
set -euo pipefail
cd "$(dirname "$0")"

N="${N:-50}"

bench() {
  local name="$1"; shift
  local s e total
  echo "=== $name x$N ==="
  s=$(date +%s%N)
  for _ in $(seq 1 "$N"); do
    "$@" >/dev/null 2>&1 || { echo "  FAILED on $name"; return 1; }
  done
  e=$(date +%s%N)
  total=$(( (e - s) / 1000000 ))
  echo "  $N runs in ${total} ms  =>  $(( total / N )) ms/iter avg  =>  $(( N * 1000 / total )) iter/s"
}

bench "hs dump (all windows)" python3 client.py dump
bench "hs dump_active"        python3 client.py dump_active

# Optional uiautomator baseline (skipped if it crashes on this device).
if adb shell "uiautomator dump /sdcard/_probe.xml" >/dev/null 2>&1 \
   && adb shell "ls /sdcard/_probe.xml" >/dev/null 2>&1; then
  bench "uiautomator dump" sh -c 'adb shell uiautomator dump /sdcard/x.xml >/dev/null && adb pull /sdcard/x.xml /tmp/uiauto.xml >/dev/null'
else
  echo "=== uiautomator dump: BROKEN on this device (exit 137); skipped ==="
fi

echo
echo "Daemon process state:"
PID=$(adb shell pgrep -f hsd | tr -d '\r' || true)
if [[ -n "${PID:-}" ]]; then
  adb shell "cat /proc/$PID/status" | grep -E 'VmRSS|Threads' | sed 's/^/  /'
fi
