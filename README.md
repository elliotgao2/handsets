<p align="center">
  <img src="logo.svg" alt="Handsets — a high-performance Android control CLI, built for agents." width="540">
</p>

<p align="center">
  <em>The device control plane you wished <code>adb</code> was.</em>
</p>

---

```bash
$ hs                                           # list devices
$ hs use                                       # connect, start daemon, mirror state
$ hs info                                      # neofetch-style snapshot (2 ms)
$ hs see x.jpg                                 # screenshot — 100× faster than adb
$ hs find 'TextView[text~=Login]'              # CSS-like selector over the live tree
$ hs open com.foo/.MainActivity && hs wait com.foo
$ hs type "EditText" "user@example.com"        # ACTION_SET_TEXT, no virtual keyboard
$ hs do <<'EOF'                                # persistent shell — many cmds, one socket
  tap_and_dump x=540 y=1500 idle_ms=200
EOF
```

Handsets is an on-device daemon plus a host CLI (`hs`) that replaces `adb`
for agent-driven and scripted automation. One persistent TCP socket. Sub-
microsecond state reads via a push-mirrored cache. No `adbd → shell →
app_process → tool` cold-start per command — the framework stays warm in
a long-lived JVM on-device.

## Install

```bash
git clone https://github.com/…/handsets && cd handsets
./build.sh                                                          # daemon jar
cargo build --release --manifest-path handsets-cli/Cargo.toml       # `hs`
cargo build --release --manifest-path handsets-viewer/Cargo.toml    # `hs see` GUI
ln -s "$PWD/handsets-cli/target/release/hs" /usr/local/bin/hs
hs use
```

## Verbs

```
hs                                  list attached devices
hs use [SERIAL]                     connect; auto-spawns the state mirror
hs drop [SERIAL] [--keep-jar]

hs info                             neofetch-style snapshot (2 ms, from local cache)
hs see                              live viewer (Metal + VideoToolbox H.264)
hs see foo.{jpg,png,xml,json}       capture, format by extension
hs ui [-i|--json|--xml] [--all]     UI tree dump — `-i` filters to readable
                                      interactive elements only (flat columnar)
hs find SELECTOR                    CSS-like:  Tag[attr=val]:flag, comma = OR
hs show [top | PKG]                 device state | top activity | package info
hs apps [--3rd]                     installed packages

hs open  COMPONENT                  start activity
hs close PKG                        force-stop
hs install APK [APK …]              streamed PackageInstaller, multi-APK
hs uninstall PKG

hs tap  "Login" | X Y               text-lookup or coords
hs type TEXT                        KeyEvents to the focused field
hs type SELECTOR TEXT               ACTION_SET_TEXT — atomic, bypasses the IME
hs go   back | home | recents | …   key events
hs swipe left|right|up|down [DUR_MS]         80% screen swipe (daemon picks coords)
hs swipe X1 Y1 X2 Y2 [DUR_MS]

hs wait idle [Nms] | TEXT | PKG | Nms        event-driven, no polling
hs cp   device:src dst   |   src device:dst  scp-style file transfer
hs prop [KEY [VAL]]                          bare = list all; KEY = get; KEY VAL = set
hs settings [NS [KEY [VAL]]]                 bare = list all 3; NS = list one;
                                               NS KEY = get; NS KEY VAL = set

hs logs [--tail N | --follow]       logcat (default last 100)
hs events                           lifecycle stream (am monitor)
hs shell                            interactive REPL (history, built-ins,
                                      unknown verbs fall through to /system/bin/sh)
hs do [WIRE]                        same REPL, or one-shot raw wire
```

## Benchmark

Same emulator, sorted by speedup. `hs` = host-side mirror file or one
warm-socket round-trip; `adb` = fresh invocation per call.

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

## Architecture

```
host                                  device (shell UID, app_process)
─────                                  ──────────────────────────────
hs <verb> ──► TCP forward ────►     Server.java
                                       ├─ State            (push-cached snapshot)
                                       ├─ Dumper Screenshot Input
                                       ├─ Files Installer  (chunked streaming)
                                       ├─ Pm Am Wm Props   (direct binder)
                                       ├─ SettingsDirect   (IContentProvider via External)
                                       ├─ Dumpsys Logcat ShellExec Lifecycle
                                       ├─ UiEvents WaitRegistry  (wait_for_*)
                                       └─ NodeActions      (AccessibilityNodeInfo)
```

The daemon runs as the shell UID via `app_process` with hidden-API
restrictions lifted. The host runs a background `state-daemon` that
subscribes to `state_watch` and atomically rewrites
`~/.handsets/state-<port>.json` on every event-driven refresh. `hs info` /
`hs show` / `hs state X` all read straight out of that file.

## Layout

```
src/dev/handsets/daemon/      on-device daemon (Java → dex → jar)
handsets-cli/                 host CLI — short-verb surface (`hs use`, `hs see`, …)
handsets-viewer/              GUI mirror — winit + Metal + zero-copy VideoToolbox
build.sh                      javac → R8 → dex → jar
```

## Sharp edges

- Settings provider rejects our `app_process` identity; `SettingsDirect`
  uses `IActivityManager.getContentProviderExternal` (the `cmd content`
  path).
- `am start` goes via `IActivityTaskManager.startActivityAsUser` with
  `callingPackage="com.android.shell"`; the system Context can't host
  `startActivity` from our anonymous process.
- `BroadcastReceiver` registrations from our Context aren't routed —
  `appCounts` cache TTL'd at 4.5 s instead.
- Most binder lookups go via reflection so the jar compiles against the
  public SDK. If a new SDK level renames a method, widen the matcher in
  the relevant helper.
