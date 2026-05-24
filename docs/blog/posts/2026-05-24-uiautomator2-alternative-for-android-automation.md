---
date: 2026-05-24
slug: uiautomator2-alternative-for-android-automation
description: Compare uiautomator2 and Handsets for Android automation, including Python workflows, latency, setup, selectors, and LLM-agent use cases.
categories:
  - Android automation
  - uiautomator2
  - Comparisons
---

# uiautomator2 Alternative for Android Automation

`uiautomator2` is a popular Python library for Android UI automation.

It is a good choice when you want a Python-native API around Android's UIAutomator framework. It can find elements by text, click buttons, inspect UI hierarchy, and drive common app flows.

But it is not always the best fit.

If your workflow is CLI-first, latency-sensitive, or driven by an LLM agent, a smaller Android automation tool may be easier to compose.

Handsets is one such alternative.

<!-- more -->

## uiautomator2 vs Handsets

| Need | uiautomator2 | Handsets |
| --- | --- | --- |
| Python API | Yes | Yes |
| CLI-first workflow | No | Yes |
| Tap by text | Yes | Yes |
| No root | Yes | Yes |
| On-device helper | `atx-agent` / app components | One small jar via `app_process` |
| Protocol | HTTP/JSON | Length-prefixed frames over `adb forward` |
| Typical tap latency | 30-100 ms | 2-7 ms after warmup |
| LLM-friendly UI table | No | Yes |
| Best fit | Python tests | Shell scripts, agents, fast automation |

Both tools can automate Android. The difference is where they are comfortable.

`uiautomator2` is comfortable inside Python test code. Handsets is comfortable at the command line.

## Python example

With `uiautomator2`, you might write:

```python
import uiautomator2 as u2

d = u2.connect()
d(text="Sign in").click()
d(text="Email").set_text("you@example.com")
d(text="Continue").click()
```

With Handsets:

```python
from handsets import Session

with Session() as d:
    d.tap("Sign in", visible=True, unique=True)
    d.fill("Email", "you@example.com")
    d.tap("Continue", visible=True, unique=True)
    d.wait(text="Dashboard", timeout="15s")
```

Or as shell:

```bash
hs tap "Sign in"
hs fill "Email" "you@example.com"
hs tap "Continue"
hs wait "Dashboard"
```

That shell shape is the main reason to choose Handsets.

## Latency difference

`uiautomator2` talks to a device-side HTTP service. That is convenient, but it adds overhead to every action.

Handsets keeps one warm device daemon and sends compact frames over a forwarded TCP port. For repeated calls, that keeps the hot path small.

Typical measured ranges:

```text
Handsets tap by label       2-7 ms
uiautomator2 click          30-100 ms
Appium click                100-500 ms
adb shell input tap         40-80 ms, coordinates only
```

If your script performs ten actions, the difference may not matter.

If your agent performs hundreds or thousands of small actions, it does.

## UI output for LLM agents

LLM agents need a compact description of the current screen.

Raw Android XML is verbose. It includes empty layout nodes, repeated boolean attributes, full class names, and bounds rectangles.

Handsets can return a compact action table:

```text
fill  EditText  "Email"     #email     540,540
fill  EditText  "Password"  #password  540,640  [password]
tap   Button    "Continue"  #continue  540,860
```

That table is easier to feed to a model. It contains the labels and actions the model needs, not the entire layout tree.

## When uiautomator2 is better

Use `uiautomator2` when:

- You want a Python-first automation library.
- Your tests already live in Python.
- You want a mature wrapper around UIAutomator.
- HTTP/JSON overhead is not a problem.
- You do not care about shell-first usage.

It is a solid tool.

## When Handsets is better

Use Handsets when:

- You want one-line CLI commands.
- You are building LLM agents.
- You need low per-action latency.
- You want to drive Android from any language via subprocess.
- You want compact UI output for prompts.
- You are writing CI smoke tests or RPA scripts.

The shortest version:

```bash
hs use
hs ui
hs tap "Continue"
```

That is the intended workflow.

## Related guides

- [Tapping Android in 5 ms](2026-05-23-tapping-android-in-5ms-vs-appium-uiautomator2.md)
- [Stop Wasting Tokens on Android Automation](2026-05-24-stop-wasting-tokens-on-android-automation.md)
- [Handsets vs Appium](2026-05-24-handsets-vs-appium.md)
