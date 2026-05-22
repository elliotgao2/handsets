<p align="center">
  <img src="logo.svg" alt="Handsets" width="540">
</p>

<p align="center">
  <em>Drive Android from the shell, in milliseconds. One jar; no app, no root.</em>
</p>

<p align="center">
  <a href="https://github.com/elliotgao2/handsets/releases/latest"><img alt="release" src="https://img.shields.io/github/v/release/elliotgao2/handsets?color=blue"></a>
  <a href="https://pypi.org/project/handsets/"><img alt="pypi" src="https://img.shields.io/pypi/v/handsets?color=blue"></a>
  <a href="LICENSE"><img alt="license: MIT" src="https://img.shields.io/badge/license-MIT-green"></a>
</p>

---

```bash
$ hs use                              # connect, start the on-device daemon
daemon up on tcp:9008

$ hs ui                               # flat table of tappable nodes
@(540,540)   click             EditText    #email        desc="Email"
@(540,640)   click,password    EditText    #password     desc="Password"
@(540,860)   click             Button      #continue     "Continue"

$ hs tap "Continue"                   # text-lookup tap
tapped "Continue" → ok

$ hs wait "Welcome"                   # block on success text
ok elapsed=412
```

`ui → label → tap → wait`. Pipe `hs ui` into a model, take the label, hand it back.

## Numbers

Same emulator, `hs` warm-socket vs fresh `adb` invocation, `n=50`:

```
hs show         0.21 µs   push-mirror local read
hs prop KEY     1.6  ms   vs adb shell getprop      46 ms   →  29×
hs show top     2.0  ms   vs dumpsys window         86 ms   →  43×
hs info         2.5  ms   vs N getprops + dumpsys  200+ ms  →  80×
hs see x.jpg    7.7  ms   vs adb exec-out screencap 705 ms  →  92×
```

Reproduce: `hs bench -n 50`. Full table and methodology in [docs/benchmark.md](docs/benchmark.md).

## How

```
host                                  device (shell UID via app_process)
─────                                  ─────────────────────────────────
  hs ─── adb forward ────► tcp:9008 ─► Server.java
                                         ├─ accessibility dump
                                         ├─ binder reflection (Pm/Am/Wm/Settings)
                                         └─ screenshots + state mirror
```

Wire frame:

```
host → device:   [u32 BE len][ascii: verb [k=v ...]]
device → host:   [u32 BE len][bytes]              ok body
                 [u32 BE 0]                       end-of-stream
                 ERR:<CODE>:<detail>              failure
```

Full protocol: [docs/wire.md](docs/wire.md). Sharp edges and reflection details: [docs/architecture.md](docs/architecture.md), [docs/sharp-edges.md](docs/sharp-edges.md).

## Install

`adb` on `$PATH`, then:

```bash
curl -fsSL https://raw.githubusercontent.com/elliotgao2/handsets/main/install.sh | bash
```

macOS and Linux. Pin a version with `HANDSETS_VERSION=v0.1.2 …`.

## Selectors

CSS-like, Playwright-inspired. A complete login flow:

```bash
hs tap   'EditText[hint~=Email]'                   --visible --unique
hs type  'EditText[hint~=Password]'                'hunter2'
hs tap   'Button:has-text("Sign in")'              --visible --unique --timeout 5s
hs wait  "Dashboard"                               --timeout 15s
```

Vocabulary: `[a=v]` `[a~=sub]` `[a^=pre]` `[a$=suf]` · `:visible :clickable :enabled :focused :checked` · `:has-text("x") :text-is("x")` · `:in(SEL) :below(SEL) :right-of(SEL) :near(SEL, PX)`. Comma is OR. Recipes: [docs/cookbook.md](docs/cookbook.md).

## vs uiautomator2, Appium

|  | **Handsets** | uiautomator2 | Appium |
|---|---|---|---|
| Single-call latency | **2–7 ms** | 30–100 ms | 100–500 ms |
| On-device install | **1 jar (~few hundred KB)** | 2 APKs + `atx-agent` | driver APK + Node server |
| Wire | TCP, length-prefixed binary | HTTP/JSON | WebDriver |
| Driven from | **any language via subprocess** | Python only | multi-lang via WebDriver |

The honest tradeoff: u2 and Appium ship recorders, pytest plugins, HTML reports. Handsets is a lean CLI.

## Exit codes

```
0  ok     1  failure     2  NOT_FOUND     3  TIMEOUT     4  AMBIGUOUS
```

Full structured `error.code` in `hs --json` output for the long tail.

## Bindings

```python
from handsets import Session                      # pip install handsets
with Session() as d:
    d.tap("Continue")
    d.wait("Welcome", timeout="15s")
```

Other languages: drive `hs --json` as a subprocess and parse one JSON line per call. ~30 lines in any host language.

## Reference

`hs --help` is the verb table.

- [Cookbook](docs/cookbook.md) — login, retry-on-flake, two-factor SMS, multi-device fan-out
- [Architecture](docs/architecture.md) · [Wire reference](docs/wire.md) · [Benchmark](docs/benchmark.md) · [Sharp edges](docs/sharp-edges.md)
- [Blog](https://elliotgao2.github.io/handsets/blog/)

## Status

Pre-1.0. CLI surface stable since v0.1.0. Wire protocol versioned via the `info` verb; clients pinning v0 should track the 0.1.x line.

## License

MIT — see [LICENSE](LICENSE).
