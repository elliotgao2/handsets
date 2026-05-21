<p align="center">
  <img src="logo.svg" alt="Handsets — a high-performance Android control CLI, built for agents and humans." width="540">
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

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/elliotgao2/handsets/main/install.sh | bash
```

macOS and Linux. Pin a version with `HANDSETS_VERSION=v0.1.0 …`. Then
`hs use` against a connected device.

<details>
<summary>Build from source</summary>

```bash
git clone https://github.com/elliotgao2/handsets && cd handsets
./build.sh                                                          # daemon jar
cargo build --release --manifest-path handsets-cli/Cargo.toml       # `hs`
cargo build --release --manifest-path handsets-viewer/Cargo.toml    # `hs see` GUI (macOS)
ln -s "$PWD/handsets-cli/target/release/hs" /usr/local/bin/hs
```

</details>

## Verbs

### Devices

```
hs                                  list attached devices
hs use   [SERIAL]                   connect; auto-spawns the state mirror
hs drop  [SERIAL] [--keep-jar]      disconnect
```

### Inspect

```
hs info                             neofetch-style snapshot (2 ms, from local cache)
hs see                              live viewer (Metal + VideoToolbox H.264)
hs see   foo.{jpg,png,xml,json}     capture, format by extension
hs ui    [-i|--json|--xml] [--all]  UI tree dump — `-i` filters to readable
                                      interactive elements only (flat columnar)
hs find  SELECTOR                   CSS-like:  Tag[attr=val]:flag, comma = OR
hs show  [top | PKG]                device state | top activity | package info
hs apps  [--3rd]                    installed packages
```

### Activity

```
hs open      COMPONENT              start activity
hs close     PKG                    force-stop
hs install   APK [APK …]            streamed PackageInstaller, multi-APK
hs uninstall PKG
```

### Input

```
hs tap   "Login" | X Y              text-lookup or coords
hs type  TEXT                       KeyEvents to the focused field
hs type  SELECTOR TEXT              ACTION_SET_TEXT — atomic, bypasses the IME
hs go    back | home | recents | …  key events
hs swipe left|right|up|down [DUR_MS]    80% screen swipe (daemon picks coords)
hs swipe X1 Y1 X2 Y2 [DUR_MS]
```

### Sync

```
hs wait  idle [Nms] | TEXT | PKG | Nms      event-driven, no polling
hs cp    device:src dst | src device:dst    scp-style file transfer
```

### System

```
hs prop     [KEY [VAL]]             bare = list all; KEY = get; KEY VAL = set
hs settings [NS [KEY [VAL]]]        bare = list all 3; NS = list one;
                                      NS KEY = get; NS KEY VAL = set
```

### Diagnostics

```
hs logs   [--tail N | --follow]     logcat (default last 100)
hs events                           lifecycle stream (am monitor)
```

### Shell

```
hs shell                            interactive REPL (history, built-ins,
                                      unknown verbs fall through to /system/bin/sh)
hs do     [WIRE]                    same REPL, or one-shot raw wire
```

## Docs

- [Architecture](docs/architecture.md)
- [Benchmark](docs/benchmark.md)
- [Sharp edges](docs/sharp-edges.md)
