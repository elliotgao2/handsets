---
date: 2026-05-23
slug: tapping-android-in-5ms-vs-appium-uiautomator2
description: Why Appium's tap() takes 100 ms and uiautomator2's takes 30 ms, and how Handsets reaches 5 ms by skipping HTTP and keeping the JVM warm.
categories:
  - Performance
  - LLM agents
---

# Tapping Android in 5 ms (vs 100 ms in Appium, 30 ms in uiautomator2)

`tap("Continue")` from Python, including the text-to-coordinates lookup
on the live accessibility tree, runs in **2–7 ms** on a recent Pixel.
For reference, the same call through `uiautomator2` is 30–100 ms; through
Appium it's 100–500 ms. Raw `adb shell input tap X Y` is 40–80 ms — and
it only takes coordinates, not text.

Why does this matter, and how does it get that low? Both questions have
the same answer, and they share an architecture diagram.

<!-- more -->

## Why a millisecond budget matters

The inner loop of any LLM-driven Android workflow is roughly four steps:

1. Read the screen (UI dump or screenshot).
2. Ask the model what to tap.
3. Tap it.
4. Wait for the next screen.

If the model thinks for 800 ms and the screen settles in 400 ms, the
*automation framework* is the part you stop thinking about — until you
write a script that taps the same button 200 times. Mine did. The model
and the device together took four minutes. Appium added twenty seconds
on top — a 9 % surcharge for typing a thin layer of HTTP requests in
front of UI Automator.

That overhead is fine for a regression suite where every call already
costs 800 ms of model time. It's brutal for any tap-heavy workload
where the framework's per-call cost dominates the wall-clock — which is
exactly the shape of an agent or an RPA flow.

I looked at where the time was going. This is what I found.

## How uiautomator2 and Appium add 50–500 ms per call

Both `uiautomator2` and Appium ultimately reach the same Android
framework — `UIAutomator` running under an instrumentation. The path
looks like this:

```
Python / WebDriver client
        │
        ▼  HTTP / WebDriver
   atx-agent  (u2)
   appium server  (Appium)
        │
        ▼  UIAutomator instrumentation
   AccessibilityNodeInfo
        │
        ▼  binder
   the device's WindowManager
```

Each call serialises a JSON command over a TCP socket, JSON-decodes on
the other end, dispatches into instrumentation, dumps (or queries) the
a11y tree, and crosses two process boundaries on the device. The HTTP
round-trip alone — even on localhost — is rarely under 5 ms. Each
instrumentation hop adds variable overhead because UI Automator is
itself running fairly complex framework code.

Measured on a recent Pixel, single-call latencies cluster like this:

```
adb shell input tap X Y                    40–80 ms     (but X Y, not text)
uiautomator2  d(text=…).click()           30–100 ms
Appium        driver.find_element(…).click()   100–500 ms
```

The Appium tail comes from the WebDriver session-management layer
between the local server and the device. `uiautomator2` skips WebDriver
and talks to `atx-agent` over plain HTTP/JSON, which is why it's a band
faster. But both still pay for:

- A fresh HTTP request per call.
- A full UI Automator dump plus filter for every text lookup.
- A round-trip just to learn that "the screen finished settling."

These are not bugs in those tools. They're the natural consequence of
choosing HTTP/WebDriver as the wire. The bill comes due when you make
thousands of calls.

## What I wanted instead

Two specific requirements made the existing tools awkward for the
workload I was running:

**An LLM-friendly UI dump.** The default `dump_hierarchy()` returns
~20 KB of XML for a typical screen. That's hundreds of tokens of layout
noise wrapping the four labels the agent actually cares about. For a
loop that dumps the screen between every step, every byte the model
doesn't need is a byte it has to read anyway.

**A latency budget under 10 ms per call.** Not for sport — because at
100 ms per call, an agent that issues four calls between every model
decision adds 400 ms of pure wait time per step. That's the difference
between a 30-second flow and a 90-second flow.

What I built is called [Handsets](https://github.com/elliotgao2/handsets).
The agent-loop surface is small:

```bash
$ hs use                              # connect, start the on-device daemon
$ hs ui                               # flat table of tappable nodes
fill  "Email"     #email     540,540
fill  "Password"  #password  540,640  [password]
tap   "Continue"  #continue  540,860
$ hs tap "Continue"                   # text-lookup tap
tapped "Continue" → ok
```

Each row reads like the next CLI call: the verb is what the agent would
issue, the label is in the quotes, the coordinates trail. An LLM picks a
label and you hand it back to `hs tap`. The UI dump for the screen above
is **3.3 KB / 729 tokens** — versus Appium's `page_source` on the same
screen at **22.3 KB / 5 762 tokens**. About 8× fewer tokens, same agent
decisions. (Full bench: [An Android UI dump for LLMs](android-ui-dump-for-llms.md).)

## Where the time went

I took three things off the critical path.

**1. The HTTP/WebDriver hop is gone.** Handsets uses a length-prefixed
binary protocol over `adb forward`:

```
host → device:   [u32 BE len][ascii: verb [k=v ...]]
device → host:   [u32 BE len][bytes]
```

A `tap x=540 y=860` is one frame in, one frame out. No JSON parser, no
HTTP headers, no session id. On a warm socket the round-trip is 2–3 ms.

**2. The JVM stays warm.** Most automation frameworks spawn or attach an
instrumentation per session. Handsets pushes one small jar to the device
(`/data/local/tmp/hs.jar`, a few hundred KB) and runs it under
`app_process` with shell UID and hidden-API restrictions lifted. That
process stays alive between calls. The first call is ~150 ms while the
daemon spins up; every subsequent call is one binary round-trip.

**3. Read-only state is mirrored to the host.** "What's the top
activity?", "What's the battery level?", "What's the screen size?" —
those don't need a device round-trip at all. A small daemon on the host
subscribes to a push stream of state changes and atomically rewrites
`~/.handsets/state-<port>.json` on each event. Reads are
**microseconds**, not milliseconds.

Here is the resulting per-call comparison versus raw `adb shell` — the
baseline most people are actually competing against:

```
hs show              0.21 µs   push-mirror local read
hs prop KEY          1.6 ms    vs adb shell getprop        46 ms    →  29×
hs show top          2.0 ms    vs dumpsys window | grep    86 ms    →  43×
hs see x.jpg         7.7 ms    vs adb exec-out screencap  705 ms    →  92×
```

Reproduce locally with `hs bench -n 50`; methodology in
[docs/benchmark.md](https://github.com/elliotgao2/handsets/blob/main/docs/benchmark.md).

## The honest tradeoff

`uiautomator2` and Appium ship things Handsets doesn't:

- Recorders that watch a human and emit code.
- pytest plugins, HTML reports, screenshot diffing.
- Cross-platform test orchestration (Appium drives iOS too).
- A huge community and a decade of Stack Overflow answers.

If you're writing a regression suite for a corporate test team, those
things matter more than 50 ms per call. If you're driving an LLM agent
loop, scripting RPA flows from bash, or running a tap-heavy workload
where the framework's per-call cost dominates wall-clock time, the
latency floor of UI Automator wrappers may be higher than your project
can afford.

The code is on [GitHub](https://github.com/elliotgao2/handsets) under
MIT. `pip install handsets` for the Python SDK, `curl … | bash` for the
CLI. macOS and Linux hosts. No root, no installed app on the phone, just
`adb` on `$PATH`.

I'd love to hear what your bench shows.
