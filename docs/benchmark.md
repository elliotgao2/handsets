# Benchmark

Same emulator, sorted by speedup. `hs` = host-side mirror file or one
warm-socket round-trip; `adb` = fresh invocation per call. Reproduce
with `hs bench -n 50` (warm-socket loop in the CLI).

| Command | hs | adb | Speedup |
|---|---:|---:|---:|
| any `hs state X` (push-mirror local read) | **0.21 µs** | 100+ ms via `dumpsys` | **~10 000×** |
| `hs see x.jpg` vs `adb exec-out screencap -p` | **7.7 ms** | 705 ms | **92×** |
| `hs info` (12-field snapshot, file read) | **2.5 ms** | 200+ ms via N getprops | **80×+** |
| `hs show top` vs `dumpsys window \| grep` | **2.0 ms** | 86 ms | **43×** |
| `hs wm-info` vs `wm size` | **2.1 ms** | 70 ms | **34×** |
| `hs prop KEY` vs `adb shell getprop` | **1.6 ms** | 46 ms | **29×** |
| `hs settings get` (direct provider) | **4.5 ms** | 69 ms | **15×** |
| `hs install --reinstall` (skips `/tmp` staging) | **2.4 s** | 3.0 s | **1.2×** |
| `hs do` shell, 100 `dump_active` calls | **0.91 s** | 1.65 s as 100 invocations | **1.8×** |
