#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

PORT="${PORT:-9008}"
JAR="build/hs.jar"
DEV_JAR="/data/local/tmp/hs.jar"
DEV_LOG="/data/local/tmp/hs.log"

if [[ ! -f "$JAR" ]]; then
  echo "error: $JAR missing; run ./build.sh first" >&2
  exit 1
fi

# Hard-kill any prior daemon and wait for binder linkToDeath to clear the
# stale UiAutomationService registration on system_server's side.
adb shell "pkill -9 -f hsd" >/dev/null 2>&1 || true
for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
  alive="$(adb shell "pgrep -f hsd || true" 2>/dev/null | tr -d '\r')"
  if [[ -z "$alive" ]]; then break; fi
  sleep 0.2
done
# Give system_server a moment to drop the previous registration.
sleep 0.5

adb push "$JAR" "$DEV_JAR" >/dev/null
adb forward --remove tcp:$PORT >/dev/null 2>&1 || true
adb forward tcp:$PORT tcp:$PORT >/dev/null

# Start the daemon detached. nohup + setsid keeps it alive after shell exits.
adb shell "CLASSPATH=$DEV_JAR nohup app_process /system/bin --nice-name=hsd dev.handsets.daemon.Main --port=$PORT > $DEV_LOG 2>&1 &"

# Wait for the listening line in the log.
for i in {1..30}; do
  if adb shell "cat $DEV_LOG 2>/dev/null" | grep -q "hsd listening"; then
    echo "daemon up on $PORT"
    adb shell "cat $DEV_LOG"
    exit 0
  fi
  sleep 0.2
done

echo "error: daemon did not come up within 6s" >&2
echo "--- log ---" >&2
adb shell "cat $DEV_LOG 2>/dev/null" >&2 || true
exit 1
