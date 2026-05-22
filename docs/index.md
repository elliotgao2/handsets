# Handsets

*A millisecond-latency CLI for driving Android devices. Built for LLM agents and shell scripts.*

Handsets is the smallest thing that gives you a tap, a type, and a wait on a real Android device, from any language. It is one Rust CLI on the host and one small jar on the device — no installed app, no root, no WebDriver server. A single call costs a few milliseconds.

This guide reads as a story. Start at the top.

---

## Why

The standard ways to drive an Android device are heavy. `adb shell input tap X Y` works for coordinates but can't find a button by label. `uiautomator2` makes you install two APKs and speaks only Python. Appium asks you to bring up a WebDriver server and then pays ~100 ms per click on top of that. None of these were designed for the case that's most common today: an LLM loop or a shell script that wants to call `tap("Continue")` thousands of times in an afternoon.

Handsets is designed exactly for that loop. You drop one jar on the device, the daemon stays warm under `app_process`, and the CLI talks to it over a length-prefixed binary protocol. A round trip is 2–7 ms. Every action verb returns a structured exit code so unattended scripts can branch without parsing stderr.

## Install

You need `adb` on `$PATH` (`brew install android-platform-tools` on macOS, the matching `android-tools` package on Linux). Then:

```bash
curl -fsSL https://raw.githubusercontent.com/elliotgao2/handsets/main/install.sh | bash
```

USB debugging on, plug the phone in, confirm `adb devices` lists it, and you're ready.

## The agent loop

This is the one passage to read if you read nothing else.

```bash
hs use                 # auto-detect a device, start the daemon
hs ui                  # flat table of tappable nodes — drop into an LLM
hs tap "Continue"      # the LLM picks a label, you tap it
hs wait "Welcome"      # block on the next screen
hs drop                # tear it all down
```

`hs ui` (since v0.1.2 the interactive table is the default) returns one line per actionable node:

```
@(540,540)   click             EditText    #email        desc="Email"
@(540,640)   click,password    EditText    #password     desc="Password"
@(540,860)   click             Button      #continue     "Continue"
```

Hand that to a model, get back a label, hand the label to `hs tap`. That's the loop. The full XML hierarchy is still one flag away (`hs ui --xml`), but for an LLM the flat table is roughly 10× cheaper in tokens.

## Selectors

When labels collide or you need a more specific match, `hs find` and the selector-aware verbs accept a CSS-like grammar. The vocabulary is borrowed from Playwright's locator API on purpose, so muscle memory transfers.

```bash
hs tap 'Button[text="Sign in"]'                   # exact text
hs tap 'Button:has-text("Sign in")'               # substring (Playwright sugar)
hs tap 'EditText:below(TextView[text=Email])'     # relational
hs tap 'Button:near(ImageView[desc~=cart], 200)'  # within 200 px
```

The full set of pseudo-classes (`:visible`, `:clickable`, `:in()`, `:below()`, `:right-of()`, `:near()`, `:has-text()`, `:text-is()`) is documented in the [cookbook](cookbook.md).

## Reliability

Every action verb honours the same set of flags so RPA scripts stop hand-rolling retry loops:

```bash
hs tap "Refresh" --visible --unique --timeout 3s --retries 4 --retry-delay 500ms
```

Failures funnel through structured exit codes:

| Code | Meaning |
| ---- | ------- |
| 0    | ok |
| 1    | failure (everything below not broken out — full code in `error.code` when `--json`) |
| 2    | NOT_FOUND |
| 3    | TIMEOUT |
| 4    | AMBIGUOUS — `--unique` saw more than one match |

That deliberate small alphabet is the thing scripts actually branch on. The full structured `ErrCode` lives in JSON-mode output for any caller that wants to dispatch on the long tail (`DAEMON_ERROR`, `SECURE_WINDOW`, `BAD_ARG`, etc.).

## JSON output and language bindings

`--json` (or `HS_FORMAT=json`) makes every action verb emit one structured line:

```json
{"verb":"tap","ok":true,"result":{"x":540,"y":860,"text":"Continue"}}
{"verb":"tap","ok":false,"error":{"code":"NOT_FOUND","detail":"no node matched"}}
```

That's the shape any subprocess driver should consume. A first-party Python SDK lives under [`bindings/python/`](https://github.com/elliotgao2/handsets/tree/main/bindings/python); other languages can drive the CLI in ~30 lines of subprocess code.

## Where to next

- **[Cookbook](cookbook.md)** — login flow, retry-on-flake, two-factor SMS, multi-device fan-out
- **[Architecture](architecture.md)** — daemon, host mirror, wire protocol
- **[Wire reference](wire.md)** — the raw protocol if you want to write your own client
- **[Sharp edges](sharp-edges.md)** — what doesn't work and why
- **[Benchmark](benchmark.md)** — full latency numbers vs uiautomator2 and Appium
- **[Blog](blog/index.md)** — long-form posts on how Handsets works

## Status

Pre-1.0. The CLI surface is stable; the wire protocol may shift between minor versions.
