# Benchmark

Measured on 2026-05-24 with `n=50` on the Android 15 emulator
`Google sdk_gphone64_arm64` (`1440x3120`, SDK 35), host macOS/aarch64.
Numbers are p50 with p95 in parentheses. `hs` uses one warm daemon socket;
`adb` starts a fresh adb CLI process per sample.

Reproduce the Handsets side with `hs bench -n 50` or machine-readable
`hs bench -n 50 --json`. Reproduce raw ADB baselines with
`scripts/bench-adb.sh -n 50`; the script keeps the exact ADB commands
inspectable.

The default `hs bench` suite follows the hot path: 768px JPEG screenshots
for agents, with WebP as a lossy export comparison. PNG is intentionally
left out of the headline `hs` suite because Android's PNG encoder is a
debug/export path, not an interaction loop path.

## Headline Comparisons

| Command | hs | adb | Speedup |
|---|---:|---:|---:|
| `screenshot size=768` JPEG vs `adb exec-out screencap -p` PNG | **8.02 ms** (10.76) | 2105.61 ms (2308.38) | **263×** |
| `screenshot max=1` JPEG vs `adb exec-out screencap -p` PNG | **52.60 ms** (71.23) | 2105.61 ms (2308.38) | **40×** |
| `wm_info` vs `adb shell wm size` | **1.34 ms** (2.29) | 45.81 ms (67.83) | **34×** |
| `state top` vs `adb shell dumpsys window` | **2.54 ms** (5.09) | 71.28 ms (107.80) | **28×** |
| `ping` vs `adb shell true` | **1.61 ms** (2.64) | 27.65 ms (36.33) | **17×** |

## Handsets Warm-Socket Suite

| Command | p50 | p95 | bytes |
|---|---:|---:|---:|
| `ping` | 1.61 ms | 2.64 ms | 4 |
| `info` | 1.64 ms | 2.80 ms | 9 |
| `wm_info` | 1.34 ms | 2.29 ms | 81 |
| `state top` | 2.54 ms | 5.09 ms | 60 |
| `wait_for_idle idle_ms=0 timeout_ms=1000` | 1.49 ms | 3.40 ms | 15 |
| `dump_active` | 4.58 ms | 6.92 ms | 5960 |
| `dump` | 6.62 ms | 9.58 ms | 10317 |
| `screenshot size=480` JPEG q80 | 3.42 ms | 6.26 ms | 12069 |
| `screenshot size=768` JPEG q80 | 8.02 ms | 10.76 ms | 22414 |
| `screenshot size=1080` JPEG q80 | 12.33 ms | 17.34 ms | 34611 |
| `screenshot size=1080 q=95` JPEG | 11.89 ms | 19.83 ms | 83811 |
| `screenshot max=1` JPEG q80 | 52.60 ms | 71.23 ms | 142014 |
| `screenshot size=768 q=95` JPEG | 6.35 ms | 10.34 ms | 49390 |
| `screenshot size=768 fmt=webp` q80 | 62.26 ms | 78.54 ms | 12090 |

## Raw ADB Baseline

| Command | p50 | p95 | bytes |
|---|---:|---:|---:|
| `adb shell true` | 27.65 ms | 36.33 ms | 0 |
| `adb shell getprop ro.build.version.sdk` | 31.38 ms | 41.57 ms | 3 |
| `adb shell wm size` | 45.81 ms | 67.83 ms | 25 |
| `adb shell settings get system screen_brightness` | 45.33 ms | 57.92 ms | 4 |
| `adb shell dumpsys window` | 71.28 ms | 107.80 ms | 87667 |
| `adb exec-out screencap -p` | 2105.61 ms | 2308.38 ms | 2024218 |
