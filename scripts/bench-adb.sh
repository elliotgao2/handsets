#!/usr/bin/env bash
set -u

N=20
SERIAL=""
JSON=0
ADB="${ADB:-adb}"

usage() {
  cat <<'EOF'
usage: scripts/bench-adb.sh [-n N] [-s SERIAL] [--json]

Fresh-process ADB baseline for the commands compared in docs/benchmark.md.
Each sample starts a normal adb CLI invocation; stdout/stderr are discarded
except for byte counts.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    -n)
      shift
      [ "$#" -gt 0 ] || { echo "-n needs a value" >&2; exit 2; }
      N="$1"
      ;;
    -s|--device)
      shift
      [ "$#" -gt 0 ] || { echo "-s needs a SERIAL" >&2; exit 2; }
      SERIAL="$1"
      ;;
    --json)
      JSON=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown arg: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

case "$N" in
  ''|*[!0-9]*)
    echo "-n must be a positive integer" >&2
    exit 2
    ;;
esac
[ "$N" -gt 0 ] || { echo "-n must be greater than 0" >&2; exit 2; }

adb_base() {
  if [ -n "$SERIAL" ]; then
    "$ADB" -s "$SERIAL" "$@"
  else
    "$ADB" "$@"
  fi
}

now_ms() {
  perl -MTime::HiRes=time -e 'printf "%.3f\n", time() * 1000'
}

json_escape() {
  awk '
    BEGIN { printf "\"" }
    {
      gsub(/\\/, "\\\\")
      gsub(/"/, "\\\"")
      gsub(/\t/, "\\t")
      gsub(/\r/, "\\r")
      printf "%s", $0
    }
    END { print "\"" }
  '
}

describe_case() {
  case "$1" in
    shell_true)      echo "adb shell true" ;;
    getprop_sdk)     echo "adb shell getprop ro.build.version.sdk" ;;
    wm_size)         echo "adb shell wm size" ;;
    settings_get)    echo "adb shell settings get system screen_brightness" ;;
    dumpsys_window)  echo "adb shell dumpsys window" ;;
    screencap_png)   echo "adb exec-out screencap -p" ;;
    *)               echo "$1" ;;
  esac
}

run_sample() {
  case "$1" in
    shell_true)      adb_base shell true ;;
    getprop_sdk)     adb_base shell getprop ro.build.version.sdk ;;
    wm_size)         adb_base shell wm size ;;
    settings_get)    adb_base shell settings get system screen_brightness ;;
    dumpsys_window)  adb_base shell dumpsys window ;;
    screencap_png)   adb_base exec-out screencap -p ;;
    *)               return 2 ;;
  esac
}

stats_for() {
  awk '
    { a[++n] = $1; sum += $1; sumsq += $1 * $1 }
    END {
      if (n == 0) {
        print "0 0 0 0 0 0"
        exit
      }
      p50 = int(n * 0.50 + 0.999999); if (p50 < 1) p50 = 1
      p95 = int(n * 0.95 + 0.999999); if (p95 < 1) p95 = 1
      p99 = int(n * 0.99 + 0.999999); if (p99 < 1) p99 = 1
      mean = sum / n
      variance = (sumsq / n) - (mean * mean)
      if (variance < 0) variance = 0
      printf "%.3f %.3f %.3f %.3f %.3f %.3f\n", a[1], a[p50], a[p95], a[p99], mean, sqrt(variance)
    }
  '
}

run_case() {
  label="$1"
  id="$2"
  samples="$(mktemp)"
  body="$(mktemp)"
  errors=0
  bytes=0

  i=0
  while [ "$i" -lt "$N" ]; do
    start="$(now_ms)"
    if run_sample "$id" >"$body" 2>/dev/null </dev/null; then
      end="$(now_ms)"
      awk -v s="$start" -v e="$end" 'BEGIN { printf "%.3f\n", e - s }' >>"$samples"
      bytes="$(wc -c <"$body" | tr -d ' ')"
    else
      errors=$((errors + 1))
    fi
    i=$((i + 1))
  done

  sorted="$(mktemp)"
  sort -n "$samples" >"$sorted"
  set -- $(stats_for <"$sorted")
  min="$1"; p50="$2"; p95="$3"; p99="$4"; mean="$5"; stddev="$6"
  rm -f "$samples" "$sorted" "$body"

  if [ "$JSON" -eq 1 ]; then
    command_json="$(describe_case "$id" | json_escape)"
    label_json="$(printf '%s' "$label" | json_escape)"
    printf '{"command":%s,"label":%s,"min_ms":%s,"p50_ms":%s,"p95_ms":%s,"p99_ms":%s,"mean_ms":%s,"stddev_ms":%s,"bytes":%s,"errors":%s}\n' \
      "$command_json" "$label_json" "$min" "$p50" "$p95" "$p99" "$mean" "$stddev" "$bytes" "$errors"
  else
    printf '%-28s  %9s  %9s  %9s  %9s  %9s  %12s  %6s\n' \
      "$label" "$min" "$p50" "$p95" "$p99" "$mean" "$bytes" "$errors"
  fi
}

if [ "$JSON" -eq 1 ]; then
  printf '{"type":"adb_bench","n":%s,"mode":"fresh_adb_process","serial":' "$N"
  if [ -n "$SERIAL" ]; then printf '%s' "$SERIAL" | json_escape; else printf 'null'; fi
  printf ',"cases":[\n'
else
  echo "fresh-process adb benchmark, n=$N per command"
  printf '%-28s  %9s  %9s  %9s  %9s  %9s  %12s  %6s\n' \
    "command" "min ms" "p50 ms" "p95 ms" "p99 ms" "mean ms" "bytes" "errors"
fi

CASES='
shell true|shell_true
getprop sdk|getprop_sdk
wm size|wm_size
settings get|settings_get
dumpsys window|dumpsys_window
screencap png|screencap_png
'

case_index=0
printf '%s\n' "$CASES" | while IFS='|' read -r label id; do
  [ -n "$label" ] || continue
  case_index=$((case_index + 1))
  if [ "$JSON" -eq 1 ] && [ "$case_index" -gt 1 ]; then
    printf ',\n'
  fi
  run_case "$label" "$id"
done

if [ "$JSON" -eq 1 ]; then
  printf ']}\n'
fi
