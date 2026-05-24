<p align="center">
  <img src="logo.svg" alt="Handsets" width="540">
  <br><br>
  <em>Drive Android from the shell, in milliseconds. One jar; no app, no root.</em>
  <br><br>
  <a href="https://github.com/elliotgao2/handsets/releases/latest"><img alt="release" src="https://img.shields.io/github/v/release/elliotgao2/handsets?color=blue"></a>
  <a href="https://pypi.org/project/handsets/"><img alt="pypi" src="https://img.shields.io/pypi/v/handsets?color=blue"></a>
  <a href="LICENSE"><img alt="license" src="https://img.shields.io/badge/license-MIT-green"></a>
</p>

---

```bash
$ hs use                              # connect, start the on-device daemon
daemon up on tcp:9008

$ hs ui                               # flat table of tappable nodes
fill  EditText  "Email"     #email     540,540
fill  EditText  "Password"  #password  540,640  [password]
tap   Button    "Continue"  #continue  540,860

$ hs tap  "Continue"                  # text-lookup tap
$ hs wait "Welcome"                   # block on success text
```

`ui → label → tap → wait`. Pipe `hs ui` into a model, take the label, hand it back.
Raw `screenshot` defaults to 768px-long-edge JPEG, the fast agent path;
`hs see out.jpg` saves a native-resolution export unless you pass `--size 768`.
Use WebP for compact lossy exports and PNG only for debug/lossless files.

|  | **Handsets** | `adb shell` | uiautomator2 | Appium |
|---|---|---|---|---|
| Single-call latency | **2–7 ms** | 40–700 ms | 30–100 ms | 100–500 ms |
| Find by label, not coords | **yes** | no | yes | yes |
| On-device install | **1 jar (~few hundred KB)** | none | 2 APKs + `atx-agent` | driver APK + Node server |
| Wire | TCP, length-prefixed binary | ADB protocol | HTTP/JSON | WebDriver |
| Driven from | **any language via subprocess** | shell / subprocess | Python only | multi-lang via WebDriver |

Reproduce with `hs bench -n 50`; methodology in [docs/benchmark.md](docs/benchmark.md). uiautomator2 and Appium ship things Handsets doesn't — recorders, pytest plugins, HTML reports, iOS support. Handsets is the lean CLI for tap-heavy work where per-call cost matters.

## Selectors

CSS-like, Playwright-inspired:

```bash
hs tap   'EditText[hint~=Email]'             --visible --unique
hs fill  'EditText[hint~=Password]'          'hunter2'
hs tap   'Button:has-text("Sign in")'        --visible --unique --timeout 5s
hs wait  "Dashboard"                         --timeout 15s
```

Vocabulary: `[a=v]` `[a~=sub]` `[a^=pre]` `[a$=suf]` · `:visible :clickable :enabled :focused :checked` · `:has-text("x") :text-is("x")` · `:in(SEL) :below(SEL) :right-of(SEL) :near(SEL, PX)`. Comma is OR. More patterns in [docs/cookbook.md](docs/cookbook.md).

## How it works

```
host                                  device (shell UID via app_process)
─────                                  ─────────────────────────────────
  hs ─── adb forward ────► tcp:9008 ─► Server.java
                                         ├─ accessibility dump
                                         ├─ binder reflection (Pm/Am/Wm/Settings)
                                         └─ screenshots + state mirror
```

Length-prefixed binary frames over `adb forward`:

```
host → device:   [u32 BE len][ascii: verb [k=v ...]]
device → host:   [u32 BE len][bytes]              ok body
                 [u32 BE 0]                       end-of-stream
                 ERR:<CODE>:<detail>              failure
```

Full protocol: [docs/wire.md](docs/wire.md). Reflection details and sharp edges: [docs/architecture.md](docs/architecture.md), [docs/sharp-edges.md](docs/sharp-edges.md).

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/elliotgao2/handsets/main/install.sh | bash
```

Needs `adb` on `$PATH`. macOS and Linux. Pin a version with `HANDSETS_VERSION=v0.1.25`.

Python bindings: `pip install handsets`.

```python
from handsets import Session
with Session() as d:
    d.tap("Continue")
    d.wait(text="Welcome", timeout="15s")
```

For other languages, drive `hs --json` as a subprocess and parse one JSON line per call.

---

Exit codes: `0` ok · `1` failure · `2` NOT_FOUND · `3` TIMEOUT · `4` AMBIGUOUS. Full structured `error.code` in `hs --json` output for the long tail.

`hs --help` is the verb table. Recipes in the [Cookbook](docs/cookbook.md), long-form posts on the [Blog](https://elliotgao2.github.io/handsets/blog/).

Pre-1.0. CLI surface stable since v0.1.0. Wire protocol versioned via the `info` verb. MIT — see [LICENSE](LICENSE).
