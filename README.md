<p align="center">
  <img src="logo.svg" alt="Handsets — a high-performance Android control CLI, built for agents and humans." width="540">
</p>

<p align="center">
  <em>A millisecond-latency CLI for driving Android devices. Built for LLM agents and shell scripts.</em>
</p>

<p align="center">
  <a href="https://github.com/elliotgao2/handsets/releases/latest"><img alt="release" src="https://img.shields.io/github/v/release/elliotgao2/handsets?color=blue"></a>
  <a href="LICENSE"><img alt="license: MIT" src="https://img.shields.io/badge/license-MIT-green"></a>
</p>

- **Fast** — 2–7 ms per call; the daemon stays warm and state is mirrored to a host file for µs reads.
- **Agent-shaped** — `hs ui` returns a flat table of tappable nodes (~10× fewer tokens than XML).
- **No app, no root** — one small jar pushed to the device; runs under shell UID via `app_process`.

> macOS or Linux host. No root. No installed app on the phone. Just `adb` on `$PATH`.

---

## The agent loop

```bash
$ hs use                              # auto-detects device, starts the daemon
daemon up on tcp:9008

$ hs ui                               # flat list of tappable nodes — drop into an LLM
@(540,540)   click             EditText    #email        desc="Email"
@(540,640)   click,password    EditText    #password     desc="Password"
@(540,860)   click             Button      #continue     "Continue"

$ hs tap "Continue"                   # text-lookup tap → coords → ACTION_CLICK
tapped "Continue" cls=android.widget.Button → ok
```

Drop `hs ui` into an LLM, get back a label, hand it to `hs tap` — that's the loop.
(Since v0.1.2, the agent-friendly flat table is the default. `hs ui --tree` gives the
indented outline; `--xml` / `--json` give the raw uiautomator-style hierarchy.)

## Why Handsets

- **vs `adb shell input tap`** — Handsets resolves "tap Continue" without the screen XML round-trip. ~100× faster on screenshot, sub-microsecond on state reads.
- **vs `uiautomator2`** — No Python client, no `atx-agent` apk; any language can drive it via `subprocess`. 2–7 ms per call vs 30–100 ms.
- **vs Appium** — No WebDriver server to start. One CLI binary. 2–7 ms vs 100–500 ms.

Full comparison table → [below](#vs-uiautomator2-appium).

## Install

Requires `adb` on `$PATH`.

- macOS: `brew install android-platform-tools`
- Debian/Ubuntu: `sudo apt-get install -y android-tools-adb`
- Arch: `sudo pacman -S android-tools`

Then:

```bash
curl -fsSL https://raw.githubusercontent.com/elliotgao2/handsets/main/install.sh | bash
```

macOS and Linux. Pin a version:

```bash
HANDSETS_VERSION=v0.1.2 curl -fsSL https://raw.githubusercontent.com/elliotgao2/handsets/main/install.sh | bash
```

Don't have adb set up yet? Enable USB debugging in *Developer options*, plug in, and confirm `adb devices` lists your phone.

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

## Quickstart

```bash
$ hs use                              # connect, start daemon, mirror state
$ hs open com.foo/.MainActivity       # launch an app
$ hs wait com.foo                     # block until it's foregrounded
$ hs ui -i                            # see what's tappable
$ hs type "EditText" "user@example.com"   # ACTION_SET_TEXT — atomic, bypasses the IME
$ hs tap "Continue"
$ hs submit                           # press the IME action key (Send / Go / Search / Done)
$ hs wait "Welcome"                   # block on success text
$ hs drop                             # tear the daemon down
```

## How it works

One small jar is pushed to the device and run under the shell UID — no installed app, no root. The CLI speaks a length-prefixed binary protocol over `adb forward`; the daemon stays warm between calls, and a host-side mirror keeps `~/.handsets/state-<port>.json` in sync via `state_watch`, so reads of cached state are sub-microsecond. See [docs/architecture.md](docs/architecture.md) for the binder/reflection details and the sharp edges.

## `hs ui` — the LLM input format

`uiautomator2`'s `d.dump_hierarchy()` and Appium's `driver.page_source` hand you the full XML — hundreds of KB per screen, mostly layout noise. `hs ui` filters and flattens the same tree to only the nodes a human or LLM can actually act on, usually **10–100× less text**:

```
@(54,160)    click             ImageButton                desc="Back"
@(540,360)                     TextView    #title         "Sign in to your account"
@(540,540)   click             EditText    #email         desc="Email"
@(540,640)   click,password    EditText    #password      desc="Password"
@(540,760)   check             CheckBox                   "Remember me"
@(540,860)   click             Button      #continue      "Continue"
@(540,960)   click             TextView                   "Forgot password?"
```

Four columns: **center coords / behaviour tags / class+id / text or desc**. Drop into an LLM, get back coords, hand them to `hs tap X Y` — done.

## Selectors

`hs find` queries the live accessibility tree with a CSS-like grammar:

```bash
hs find 'Button[text="Sign in"]'                       # exact text match
hs find 'EditText[rid~=email]'                         # id contains "email"
hs find 'TextView[text~=Login]:clickable'              # flag filter
hs find 'Button[text="OK"], Button[text="Continue"]'   # comma = OR
```

Relational pseudo-classes anchor matches to nearby or containing nodes:

```bash
hs find 'EditText:below(TextView[text=Email])'         # field under a label
hs find 'Button:near(ImageView[desc~=cart], 200)'      # within 200 px
hs find 'Button[text=OK]:in(LinearLayout[id~=dialog])' # inside a container
```

More patterns and end-to-end recipes live in [docs/cookbook.md](docs/cookbook.md).

## RPA essentials

```
hs run  [SCRIPT|-]                  batch CLI verbs over one warm socket
hs init [PATH]                      scaffold a starter script.hs
hs act  --tap "X" --until "Y" …     one-shot tap-then-verify composite
hs fan  S1,S2 -- VERB ARGS          run VERB in parallel on each device
```

Every action verb (`tap`, `type`, `find`, `wait`, `submit`, `paste`, `act`) honours a shared flag set, so RPA scripts stop reimplementing retry/filter loops:

| Flag                                  | Purpose                                                |
| ------------------------------------- | ------------------------------------------------------ |
| `--timeout MS`                        | Per-call wait budget (overrides 10 s default)          |
| `--retries N` / `--retry-delay MS`    | Retry on failure                                       |
| `--visible` / `--clickable` / `--enabled` | Filter the match set                               |
| `--unique` / `--nth I`                | Disambiguate multiple matches                          |
| `--json` (or `HS_FORMAT=json`)        | `{"verb":…, "ok":…, "result"\|"error":…}` per line     |
| `--fresh`                             | Force a re-dump inside `hs run`/`shell`                |

Failures map to a small set of exit codes (`2 NOT_FOUND`, `3 TIMEOUT`, `4 AMBIGUOUS`; everything else collapses to `1`), so scripts can branch on `$?` without parsing stderr. The full structured `ErrCode` is preserved in `--json` output as `error.code` for callers that need fine-grained dispatch. Full recipes (login flow, retry-on-flake, two-factor SMS, multi-device fan-out, …) in [docs/cookbook.md](docs/cookbook.md).

<details>
<summary><strong>All verbs (click to expand)</strong></summary>

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
hs ui    [-i|--json|--xml] [--all]  UI tree dump — `-i` returns a flat,
                                      LLM-friendly table of tappable nodes
                                      (coords + tags + class + label)
hs find  SELECTOR                   CSS-like:  Tag[attr=val]:flag, comma = OR
hs show  [top | PKG]                device state | top activity | package info
hs apps  [--3rd]                    installed packages
hs links PKG                        deeplink URI templates declared by PKG —
                                      parses AndroidManifest.xml directly,
                                      so it sees everything `pm dump` does
hs sms      [inbox|sent|all]   [--limit N] [--json]    recent SMS
hs calls    [in|out|missed|all] [--limit N] [--json]   call log
hs contacts                       [--limit N] [--json] contact list
hs calendar [--days N | --from MS --to MS] [--limit N] [--json]
                                                       next 7 days of events
hs notif    [PKG] [--history] [--limit N] [--json]     active notification tray
hs clip                              read primary clipboard text
hs clip  TEXT                        write TEXT to primary clipboard
hs clip  --watch [--interval MS]     stream clipboard changes (one per line)
```

The four user-data verbs go directly through `IContentProvider` via `getContentProviderExternal` (the same hidden API path `cmd content` uses), so they don't need an installed app or `pm grant`. Output is a columnar table by default; `--json` re-emits a `[{col: val, …}, …]` array.

Example `hs info` output:

```
                Pixel 6 Pro
                ───────────
    \       /   OS        Android 15 — SDK 35
     \ ___ /    Kernel    6.1.75-android14-11
      /   \     Uptime    2d 4h 30m
     | o o |    Display   1440 × 3120
     |  _  |    CPU       8× arm64-v8a
      \___/     Memory    5.2 / 12.0 GiB
     |     |    Battery   78%  (screen on, charging)
     |_____|    Top       com.android.launcher3/.HomeActivity
      |   |     Daemon    hsd on tcp:9008
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
hs submit [SELECTOR]                press the IME submit / search / go / done
                                      key on the focused (or matched) field
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

Raw wire reference: see [docs/wire.md](docs/wire.md).

</details>

<a id="vs-uiautomator2-appium"></a>

## vs `uiautomator2`, Appium

Handsets wins on two axes:

1. **Latency** — a few milliseconds per call, versus tens to hundreds. Both alternatives wrap [UIAutomator](https://developer.android.com/training/testing/other-components/ui-automator) and pay for the framework + an HTTP / WebDriver hop on every call. Handsets keeps a JVM daemon warm under `app_process` and a state mirror on the host, so reads of cached state are sub-microsecond.
2. **Scriptable from anywhere** — a single CLI binary that any shell, Makefile, language, or LLM agent loop drives via `subprocess`. No Python-only client, no Node server to start.

Same "tap the Login button" task, three styles:

```bash
# Handsets — one CLI call, language-agnostic
hs tap "Login"
```

```python
# uiautomator2 — Python only
import uiautomator2 as u2
u2.connect()(text="Login").click()
```

```python
# Appium — start a WebDriver server first
from appium import webdriver
d = webdriver.Remote("http://127.0.0.1:4723", caps)
d.find_element("xpath", "//*[@text='Login']").click()
```

|  | **Handsets** | uiautomator2 | Appium |
|---|---|---|---|
| Single-call latency | **2–7 ms** typical | ~30–100 ms | ~100–500 ms |
| Daemon start | **< 200 ms** via `app_process`, no UIAutomator framework | UIAutomator instrumentation each session | UIAutomator + WebDriver bridge |
| State reads | **µs from host-mirrored file** (`hs info` / `hs show`) | ms per round-trip | ms+ per round-trip |
| UI dump for agents | `hs ui` flat, **~10× fewer tokens** | full XML | full XML |
| On-device install | **push 1 jar** (~few hundred KB) | 2 apks + `atx-agent` | driver apk + Node server |
| Wire | TCP + length-prefixed binary | HTTP/JSON via `atx-agent` | WebDriver over HTTP |
| Selector | CSS-like `Tag[attr=val][attr~=sub]:flag` | `d(text=…, className=…)` chained | Selenium strategies |
| Bound to | **any language via subprocess** | Python only | multi-lang via WebDriver |
| Best at | LLM agents, ad-hoc scripts, high-freq small ops | Python device scraping | cross-platform CI suites |

Honest tradeoff: uiautomator2 and Appium ship with recorders, IDE integrations, pytest runners, HTML reporting. Handsets is a lean CLI. For pytest-style UI regression with reports they're still the smoother path. Handsets is built for the case where you only care about single-call latency and shell composition.

## Status

Pre-1.0. The CLI surface is stable; the wire protocol may shift between minor versions. See [docs/wire.md](docs/wire.md).

## Language bindings

A first-party Python SDK lives under [`bindings/python/`](bindings/python/) — `pip install handsets`, then:

```python
from handsets import Session
with Session() as d:
    d.tap("Continue")
    d.wait("Welcome", timeout="15s")
```

For other languages, drive `hs --json` as a subprocess and parse one JSON line per call. ~30 lines in any host language.

## Docs

- [User guide](docs/index.md) — read this first
- [Cookbook](docs/cookbook.md) — RPA recipes (login, retry, fan-out, etc.)
- [Architecture](docs/architecture.md)
- [Wire reference](docs/wire.md)
- [Sharp edges](docs/sharp-edges.md)
- [Benchmark](docs/benchmark.md)
- [Blog](https://elliotgao2.github.io/handsets/blog/) — long-form posts on how Handsets works

## License

MIT — see [LICENSE](LICENSE).
