---
date: 2026-05-25
slug: tap-android-buttons-by-text-command-line
description: "Tap Android buttons by text from the command line without hard-coded coordinates, root, or Appium."
categories:
  - Android automation
  - ADB
  - No root
---

# How to Tap Android Buttons by Text from the Command Line

The usual command-line way to tap Android is coordinates:

```bash
adb shell input tap 540 860
```

That works, but it is not what you mean.

You mean:

```bash
tap "Continue"
```

A good Android automation script should say that directly. It should find the visible button, tap its center, and fail clearly if the button is missing or ambiguous.

Handsets gives you that command-line workflow.

<!-- more -->

## Quick answer

Install Handsets, connect the device, and tap by label:

```bash
hs use
hs tap "Continue"
```

For safer scripts:

```bash
hs tap "Continue" --visible --unique --timeout 5s
```

That taps the visible UI node whose label matches `Continue`. If more than one visible match exists, `--unique` makes the command fail instead of guessing.

## Why not use coordinates?

Coordinate taps are fragile because they describe pixels, not UI.

This command:

```bash
adb shell input tap 540 860
```

does not know whether it is tapping:

- A button.
- Empty space.
- The keyboard.
- A list item.
- A different control on another device.

If the layout shifts, the script silently does the wrong thing.

Text taps are more stable because they target the user-facing label.

## Inspect the current screen

Before tapping, you can inspect the current UI:

```bash
hs ui
```

Example output:

```text
fill  EditText  "Email"     #email     540,540
fill  EditText  "Password"  #password  540,640  [password]
tap   Button    "Continue"  #continue  540,860
```

This output is meant to be readable by both humans and automation. It shows what can be filled, tapped, or waited on.

## Tap common labels

```bash
hs tap "Sign in"
hs tap "Continue"
hs tap "Allow"
hs tap "Done"
```

When a label is repeated, tighten the selector:

```bash
hs tap 'Button:has-text("Continue")' --visible --unique
```

Or choose a specific occurrence:

```bash
hs tap "Continue" --visible --nth 2
```

Prefer `--unique` for unattended jobs. Use `--nth` only when the repeated layout is intentional and stable.

## Tap after scrolling

For long screens, use a small loop:

```bash
for _ in $(seq 1 10); do
  hs tap "Settings" --visible --unique --timeout 500ms && break
  hs swipe up 250
done
```

The script tries to tap first, then scrolls only if the target is not visible.

## Tap and verify

The most reliable scripts do not just tap. They tap and wait for the expected result:

```bash
hs tap "Continue" --visible --unique
hs wait "Dashboard" --timeout 15s
```

Or as one action:

```bash
hs act --tap "Continue" --until "Dashboard" --timeout 15s
```

That pattern avoids fixed sleeps and catches failed transitions.

## Use from Python or other languages

The command-line interface is easy to call from any language:

```python
import subprocess

subprocess.run(
    ["hs", "tap", "Continue", "--visible", "--unique", "--timeout", "5s"],
    check=True,
)
```

For Python, the first-party package is cleaner:

```python
from handsets import Session

with Session() as d:
    d.tap("Continue", visible=True, unique=True, timeout="5s")
```

## FAQ

### Can adb tap by text?

Raw `adb shell input tap` cannot tap by text. It taps coordinates. To tap by visible text, use a UI-aware layer such as Handsets, Appium, or uiautomator2.

### Does tapping by text require root?

No. Handsets taps by text through normal Android debugging access and the shell user. The device does not need to be rooted.

### What happens if there are two matching buttons?

Use `--unique`. If multiple visible nodes match, the command fails with an ambiguity error instead of choosing randomly.

### Can I tap text inside any app?

You can tap normal accessible Android UI. Apps with custom rendering or inaccessible controls may require screenshots or more specific selectors.

## Related guides

- [A Terminal UI for Driving Android Apps](2026-05-25-android-terminal-ui.md)
- [Fast Android UI Automation with ADB](2026-05-25-fast-android-ui-automation-with-adb.md)
- [How to Automate Android Without Appium](2026-05-24-how-to-automate-android-without-appium.md)
- [Python Android Automation Without Root](2026-05-24-python-android-automation-without-root.md)
