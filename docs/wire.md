# Wire reference

The on-device daemon speaks a length-prefixed binary protocol: each
frame is `uint32 big-endian length` + payload (in both directions).
Commands are ASCII text, `verb [k=v …]` style. This document lists
every wire verb the daemon dispatches in
`src/dev/handsets/daemon/Server.java`.

Use these directly via `hs do <wire>` or `hs shell` (one socket, many
commands). `ERR:<msg>` is returned on failure.

## Lifecycle

```
ping                                                  → pong
info                                                  → "<srcW> <srcH>"
quit                                                  → bye   (daemon exits)
```

## Inspect — accessibility tree

```
dump                                                  full JSON dump, every window
dump_active                                           active window only
```

## Capture — screenshots and streams

`screenshot` returns one frame; the `stream_*` verbs hold the socket
open and push continuously.

```
screenshot       [size=N] [q=N] [fmt=jpeg|png] [max=1]
stream           [size=N] [q=N] [fps=N]                  JPEG, native by default
stream_h264      [size=N] [max=1] [fps=N] [bitrate=KBPS] [gop=SEC]
stream_tilejpeg  [size=N] [q=N] [tile=N]
keyframe                                                 force IDR on all H.264 streams
```

`size=N` = long edge in pixels. `q` clamps to [1, 100], default 80.
`max=1` = native resolution (overrides `size`).

## Input

```
tap         x=NUM y=NUM
swipe       x1=NUM y1=NUM x2=NUM y2=NUM [dur=MS]
swipe_dir   left|right|up|down [dur=MS]                  60% travel from screen centre
down        x=NUM y=NUM                                  pointer down (sticks)
move        x=NUM y=NUM
up          x=NUM y=NUM                                  releases the down pointer
scroll      x=NUM y=NUM dy=NUM                           wheel scroll at point
key         NAME                                         e.g. BACK, HOME, ENTER
key         code=N                                       raw KeyEvent code
text        STRING                                       KeyEvents to the focused field
```

## Waits — event-driven, no polling

```
wait_for_idle      [idle_ms=200] [timeout_ms=5000]     → ok elapsed=N | ERR:timeout
wait_for_text      text="…" [match=sub|exact] [timeout_ms=5000]
                                                       → ok x=… y=… w=… h=… elapsed=…
wait_for_activity  n=COMPONENT [timeout_ms=5000]       prefix-matches the package
```

## Composites

Tap then await the UI to settle — saves a wire round-trip versus
`tap` + `wait_for_idle`.

```
tap_and_dump   x=NUM y=NUM [idle_ms=200] [timeout_ms=2000]   → dump_active JSON
tap_and_settle x=NUM y=NUM [idle_ms=200] [timeout_ms=2000]   → ok elapsed=N
```

## State — push-mirrored snapshot

`state` reads a single field from the cached snapshot the daemon
maintains via accessibility / lifecycle hooks. `state_watch` keeps the
socket open and pushes a fresh JSON frame on every state change.

```
state interactive | battery_level | battery_charging | top | procs | device | device_fresh
state_watch                                            chunked JSON frames
```

## Node actions — AccessibilityNodeInfo

`<selector>` uses the same CSS-like syntax as `hs find`:
`Tag[attr=val][attr~=sub]:flag`, comma = OR.

```
node_click       <selector>
node_long_click  <selector>
node_set_text    <selector> value="STRING"     atomic ACTION_SET_TEXT
node_scroll      <selector> [dir=forward|backward|up|down|left|right]
node_focus       <selector>
submit           [<selector>]                  ACTION_IME_ENTER on focused
                                               EditText (or matched selector) —
                                               fires the field's IME action
                                               (Search / Go / Send / Done / …)
```

## Packages

```
pm_list   [3] [s]                       3 = third-party only, s = system only
pm_path   PKG
pm_uninstall PKG
pm_grant  PKG PERM
pm_revoke PKG PERM
install   size=N [reinstall=1] [grant=1]           then stream APK chunks → ok | ERR
install_multi sizes=N1,N2,… [reinstall=1] [grant=1] then stream APKs concatenated
```

## Activities

```
am_start       n=COMPONENT [a=ACTION] [d=DATA] [f=FLAGS]
am_force_stop  PKG
am_kill        PKG
am_broadcast   [n=COMPONENT] [a=ACTION] [d=DATA]
```

## Files

`pull` streams `[len][chunk]…[len=0]` back. `push` expects the same
shape from the client after the header.

```
pull   path=PATH                                       → chunked stream
push   path=PATH size=N [mode=0NNN]                    client streams chunks → ok | ERR
```

## Props / settings / system

```
getprop      KEY
setprop      KEY VALUE
settings_get NS KEY                                    NS ∈ {system, secure, global}
settings_put NS KEY VALUE
wm_info                                                display + rotation summary
wm_rotation  N                                         force rotation
```

## Diagnostics

```
dumpsys SERVICE [ARGS…]                                chunked stream
logcat  [ARGS…]                                        chunked stream
shell   ARGV…                                          chunked stream + `__exit__ N` trailer
monitor                                                am-monitor lifecycle stream
```

## Quoting and escaping

- Arguments are split on ASCII whitespace.
- `"..."` quoted strings are honoured for `text="…"` / `value="…"`
  and similar — useful when the value contains spaces or `=`.
- Selectors are passed as-is; quote any embedded whitespace.

## Errors

Every failure path returns a single frame whose payload starts with
`ERR:` followed by a short tag (e.g. `ERR:tap_and_dump-needs-x-y`).
Streamed responses send the `ERR:` frame then the `len=0` terminator.
