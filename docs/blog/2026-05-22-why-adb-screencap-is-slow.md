# Why `adb screencap` is slow

*2026-05-22*

`adb shell screencap -p` is the default way to grab an Android screen from
a host machine. On a stock Android 15 emulator at 1440×3120, it takes a
median of **2.12 seconds** per call — with individual calls ranging from
660 ms to over 3 seconds.

That's fine for a debugging screenshot. It's a disaster if you're a UI
automation loop or an LLM agent that takes a screenshot between every
step.

The same screen, captured through `hs see` at the default settings, takes
**12 ms** — about 170× faster. But "170× faster" is a fragile claim if
the comparison isn't honest. Here's where the time actually goes, and
what Handsets does about it.

---

## The numbers

All measurements: Android 15 emulator (`sdk_gphone64_arm64`, 1440×3120,
SDK 35). Each variant warmed (3 calls) then sampled 20 times. Wall-clock
end-to-end from the host's perspective.

| variant                                       | median   | p10  | p90  | output  |
| --------------------------------------------- | -------: | ---: | ---: | ------: |
| `adb exec-out screencap -p > x.png`           | 2122 ms  |  661 | 3007 | 1973 KB |
| `adb exec-out screencap > x.raw`              |  675 ms  |  664 |  683 | 17.5 MB |
| `hs see x.png`         (PNG, full res)        |  584 ms  |  569 |  594 | 1978 KB |
| `hs see x.jpg`         (JPEG q80, full)       |   24 ms  |   23 |   25 |  138 KB |
| `hs do 'screenshot'`   (JPEG q80, 768 long)   | **12 ms** | **11** | **14** |  22 KB |

The first row is the apples-to-apples adb baseline most people compare
against. The last row is what an agent loop actually uses. The middle
rows let us isolate where each saving comes from.

A few observations even before any explanation:

- `screencap -p` has a **huge variance** — almost 5× between p10 and p90.
- `screencap` *without* `-p` (raw RGBA) is consistent at ~675 ms even
  though it ships 17.5 MB instead of 2 MB. Whatever causes the variance
  is not transport bandwidth.
- All the `hs` paths are tight (~5% spread). Whatever's happening in
  the warm daemon is deterministic.

## Where does `adb screencap -p` spend its time?

To break it apart, run `screencap` on the device only — capturing to a
file rather than piping to host stdout — so we can see what's actually
happening down there.

| phase                                                | median  |
| ---------------------------------------------------- | ------: |
| `screencap    /sdcard/x.raw`   (capture + raw write) |   75 ms |
| `screencap -p /sdcard/x.png`   (capture + PNG encode)|  624 ms |
| `adb pull /sdcard/x.raw`       (17.5 MB transport)   |   60 ms |
| `adb pull /sdcard/x.png`       (2 MB transport)      |   25 ms |

An on-device capture takes about 75 ms — that's the cost of SurfaceFlinger
producing a frame. Adding `-p` adds **~549 ms** of pure PNG encoding on
the device's CPU. Transport is in the noise.

In other words, **almost 90% of `adb screencap -p`'s on-device time is
PNG encoding** — single-threaded zlib-style compression on the slowest
CPU in the system, for an image you're probably going to delete in five
seconds.

That accounts for the 660-ms best case. The other ~1500 ms of variance
in `adb exec-out screencap -p` shows up on top of that, and is a
combination of:

1. `screencap` being spawned cold each call (fresh process, fresh
   SurfaceFlinger client connection).
2. `adb exec-out` piping the PNG byte-by-byte through several buffering
   layers (the on-device adbd, the USB transport, the host adb server,
   your shell's stdout).
3. The emulator's VM doing whatever else it does.

On a physical device the variance is smaller but the PNG floor doesn't
move.

## Three reasons it's slow

**1. PNG encoding is wildly mismatched to the use case.**
PNG is a great archive format. It's a terrible "snapshot a frame for an
LLM" format. JPEG q80 of the same image is ~14× smaller, and orders of
magnitude faster to encode (Skia's JPEG path uses libjpeg-turbo with
NEON SIMD; the PNG path is software zlib with no comparable
acceleration). No agent loop cares about lossless screenshots — they
care about "what's on the screen right now."

**2. `screencap` is a fresh process every call.**
The binary starts, opens the SurfaceFlinger client, captures, encodes,
exits. For one screenshot that's fine. For an agent doing 20 screenshots
a minute, you're paying process startup and SurfaceFlinger handshake
every single time.

**3. `adb exec-out` pipes through several layers that don't love 2 MB.**
The PNG comes out of `screencap` in one fwrite, gets chunked by adbd
into TCP/USB frames, goes through your host adb server, and arrives at
your shell as stdout. Each hop has buffering that under load doesn't
combine well. This is the variance source: best case 660 ms, worst 3 s.

## What Handsets does differently

Three things, each of which fixes one of the problems above.

### 1. A warm VirtualDisplay mirror in a long-running daemon

`hs use` spawns a small JVM process on the device under `app_process`
(shell UID, hidden-API restrictions lifted). The daemon creates a
[`VirtualDisplay`](https://developer.android.com/reference/android/hardware/display/VirtualDisplay)
that mirrors the default display into an
[`ImageReader`](https://developer.android.com/reference/android/media/ImageReader)
at a configurable resolution, and keeps it open between commands.

When you call `hs see x.jpg`, the latest frame is already sitting in
memory. There's no SurfaceFlinger snapshot to wait for, no `screencap`
process to start. The daemon acquires the most recent `Image` from the
ImageReader (already produced asynchronously by the listener thread on
the previous frame), JPEG-encodes it, and ships the bytes back.

The relevant detail in the [mirror code](../../src/dev/handsets/daemon/Screenshot.java)
is that the listener thread does the expensive
`copyPixelsFromBuffer` — the GPU-fence-blocking call — *without* holding
the capture lock, then briefly takes the lock just to swap pointers.
Capture threads only ever read the most recent fully-written bitmap and
never wait on GPU work. A first call at a new resolution pays a one-time
~50 ms cost to create the mirror; the cache holds the four most-recent
sizes.

### 2. JPEG is the default; PNG is opt-in

`hs see x.jpg` is JPEG. `hs see x.png` is PNG. The file extension picks
the format. Agents get JPEG by default because we know what they're
going to do with it: ship it to a model. Debugging screenshots can ask
for PNG.

This single change accounts for most of the win: 24 ms (`hs see x.jpg`)
vs 584 ms (`hs see x.png`). Both go through the same warm mirror at full
resolution; the only difference is the encoder.

### 3. Default to 768-long-edge for the agent loop

Most LLM agents don't need 1440×3120 pixels. They need "enough to see
the screen." The raw wire command `screenshot` (without `max=1`) defaults
to a 768-long-edge JPEG, which is 22 KB on disk and 12 ms end-to-end.

The downscale happens inside the mirror itself — the VirtualDisplay is
created at the *output* resolution, so we're not allocating a 1440×3120
bitmap just to throw 80% of it away. Bigger sizes have their own warm
mirror cached separately (`hs see x.jpg` triggers the full-res mirror),
so you can mix and match without paying for the largest one every time.

## Layer-by-layer wins

Adding up the changes:

| change                                  | saves                                                     |
| --------------------------------------- | --------------------------------------------------------- |
| Warm VirtualDisplay vs cold `screencap` | ~75 ms per call (skips the capture)                       |
| JPEG q80 vs PNG                         | ~550 ms per call (encode dominates)                       |
| TCP forward vs `exec-out` pipe          | ~1500 ms when adb is in a bad mood (variance)             |
| 768-long-edge default                   | another ~10 ms (smaller encode + smaller transport)       |

The first three together get you to `hs see x.jpg`'s 24 ms. The last
shaves the agent default to 12 ms.

## When this matters (and when it doesn't)

**Matters** if you're:

- An LLM agent that screenshots after every action (typical loop: act,
  screenshot, dump UI, decide, repeat).
- A test framework that wants to record a frame every X ms.
- A monitoring system polling for visual changes.
- Anything where screenshot latency is on the user-perceived path.

**Probably doesn't matter** if you're:

- Taking one screenshot a day for a bug report.
- Recording a video — `hs see` (the bare GUI viewer) uses MediaCodec
  H.264 streaming via [`H264Streamer.java`](../../src/dev/handsets/daemon/H264Streamer.java),
  a separate path.
- Working over a slow remote `adb tcpip` link where the wire, not the
  encode, is the bottleneck.

For the agent case specifically: a 12-ms screenshot lets you treat
screenshots as free relative to the rest of the loop (UI dump is ~150
ms, an LLM round-trip is seconds). Two-second screenshots make
screenshotting the dominant cost, and you start skipping them — at
which point your agent gets flakier.

## Caveats

- These numbers are from an emulator. Physical devices have lower
  variance on the `adb exec-out` path (typically 600–900 ms instead of
  2–3 s) and faster on-device JPEG encoding. The relative ordering
  doesn't change.
- The first call after a resolution change pays a one-time ~50 ms cost
  to create the new VirtualDisplay mirror.
- If a foreground window has `FLAG_SECURE`, both adb and Handsets
  produce an all-black frame. Handsets detects this and returns a
  named error pointing at the offending window instead of silently
  handing you a black PNG.
- The daemon runs as the shell UID via `app_process`, with hidden-API
  restrictions lifted. That's necessary because `createVirtualDisplay`
  rejects the system Context's op-package on Android 14+; we have to
  forge a `com.android.shell` package context. The init comments in
  `Screenshot.java` explain the three-tier strategy.

## Reproducing

```bash
# Set up
curl -fsSL https://raw.githubusercontent.com/elliotgao2/handsets/main/install.sh | bash
hs use

# Headline benchmark
python3 - <<'PY'
import statistics, subprocess, time
def t(cmd):
    for _ in range(3): subprocess.call(cmd, shell=True, stdout=subprocess.DEVNULL)
    s = []
    for _ in range(20):
        t0 = time.perf_counter_ns()
        subprocess.call(cmd, shell=True, stdout=subprocess.DEVNULL)
        s.append((time.perf_counter_ns() - t0) / 1e6)
    s.sort()
    return statistics.median(s)
print(f"adb -p:     {t('adb exec-out screencap -p > /tmp/a.png'):.1f} ms")
print(f"hs jpg:     {t('hs see /tmp/h.jpg'):.1f} ms")
print(f"hs default: {t(\"hs do 'screenshot' > /tmp/h2.jpg\"):.1f} ms")
PY
```

If you reproduce something materially different, open an issue with
your device model and Android version — the numbers above are honest
but I'm curious where they hold and where they don't.

---

*[Handsets](https://github.com/elliotgao2/handsets) is a CLI for driving
Android devices, built for LLM agents and shell scripts. MIT.*
