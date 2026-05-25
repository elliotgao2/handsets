---
date: 2026-05-24
slug: how-to-automate-android-without-appium
description: "How to automate Android without Appium: use a lightweight CLI to tap by text, fill fields, wait for screens, and run no-root scripts."
categories:
  - Android automation
  - Appium
  - No root
---

# How to Automate Android Without Appium

You do not need Appium for every Android automation task.

Appium is the right tool when you need a full WebDriver-based mobile testing framework. But many Android workflows are smaller than that. You may only need to open an app, tap visible buttons, type into fields, wait for a result, and collect a screenshot on failure.

For those jobs, a CLI can be enough.

Handsets lets you automate Android from the terminal without root and without installing a visible helper app on the phone.

<!-- more -->

## Quick answer

The fastest way to automate Android without Appium is:

```bash
hs use
hs tap "Continue"
hs fill "Email" "you@example.com"
hs wait "Dashboard"
```

That gives you the core automation loop: connect to the device, act on visible UI labels, and wait for the next state. You still use normal Android debugging access. You do not need root, WebDriver, or an Appium server.

## What you need

You need:

- An Android phone or emulator.
- USB debugging enabled.
- `adb` on your path.
- Handsets installed on your host machine.

Install:

```bash
curl -fsSL https://raw.githubusercontent.com/elliotgao2/handsets/main/install.sh | bash
```

Connect:

```bash
hs use
```

Now you can control the device with commands.

## Tap by text

Raw `adb` can tap coordinates:

```bash
adb shell input tap 540 860
```

That is brittle. It depends on screen size, density, orientation, and layout.

With Handsets, tap the visible label:

```bash
hs tap "Continue"
```

Use `--visible` and `--unique` when a script should fail rather than guess:

```bash
hs tap "Continue" --visible --unique --timeout 5s
```

This is the main difference from raw `adb shell input tap`. The script says what it means. It does not encode where a button happened to be on one device.

## Fill fields

Use `fill` for text fields:

```bash
hs fill "Email" "you@example.com"
hs fill "Password" "$PASSWORD"
hs tap "Sign in"
```

If labels are repeated, use selectors:

```bash
hs fill 'EditText:below(TextView[text=Email])' "you@example.com"
hs fill 'EditText:below(TextView[text=Password])' "$PASSWORD"
```

The selector syntax is CSS-like and built for real Android UI trees.

## Wait for the next screen

Do not write sleeps unless you truly need a fixed delay.

Instead of:

```bash
hs tap "Continue"
sleep 5
```

Use:

```bash
hs tap "Continue"
hs wait "Dashboard" --timeout 15s
```

This makes the script faster on fast devices and more reliable on slow devices.

## A complete script

Here is a no-Appium Android login script:

```bash
#!/usr/bin/env bash
set -euo pipefail

hs use
hs go com.example.app
hs wait "Sign in" --timeout 15s

hs fill "Email" "$APP_EMAIL"
hs fill "Password" "$APP_PASSWORD"
hs tap "Continue" --visible --unique
hs wait "Dashboard" --timeout 20s
```

Add failure artifacts:

```bash
ARTIFACTS="/tmp/android-run-$$"
mkdir -p "$ARTIFACTS"
trap 'hs ui > "$ARTIFACTS/ui.txt"; hs see --size 768 "$ARTIFACTS/screen.jpg"; hs logs --tail 200 > "$ARTIFACTS/logs.txt"; echo "$ARTIFACTS"' ERR
```

Now the script leaves behind the UI, screenshot, and logs when something breaks.

## Can I just use adb?

Sometimes, yes.

Raw `adb` is great for low-level device commands:

```bash
adb shell am start -n com.example/.MainActivity
adb shell input keyevent BACK
adb shell wm size
```

It becomes awkward when you need semantic UI automation:

- Tap the button labeled "Continue".
- Fill the password field below "Password".
- Wait until "Dashboard" appears.
- Fail if there are two matching buttons.

That is where a higher-level tool helps. Handsets still uses `adb` underneath, but it gives you label-based actions and structured failure modes.

## When Appium is still better

Use Appium if you need:

- iOS support.
- WebDriver compatibility.
- A full test framework.
- Cloud device farm integrations.
- Rich reports and recorders.
- A large QA ecosystem.

Those are real strengths.

But if your goal is Android-only CLI automation, Appium may be more stack than you need.

## Why this matters for LLM agents

LLM-driven Android automation benefits from a small text interface.

Handsets can print the current screen as an action table:

```text
fill  EditText  "Email"     #email     540,540
fill  EditText  "Password"  #password  540,640  [password]
tap   Button    "Continue"  #continue  540,860
```

That is easier for a model to consume than a full XML tree. It also reduces prompt size for long trajectories.

## Summary

You can automate Android without Appium if your workflow is:

- Android only.
- CLI-first.
- Label-based.
- No-root.
- Scripted from shell or Python.

Start with:

```bash
hs use
hs ui
hs tap "Continue"
hs wait "Welcome"
```

That covers more Android automation work than you might expect.

## FAQ

### Do I need Appium to automate Android?

No. Appium is useful for full mobile test frameworks, especially cross-platform suites, but Android can also be automated from the command line with `adb` and tools like Handsets.

### Can I automate Android without root?

Yes. For normal UI automation, root is not required. You can tap, type, swipe, inspect visible UI, wait for text, and capture screenshots when the current app allows it.

### Is this better than Appium?

It depends. Handsets is better for Android-only CLI scripts, LLM agents, and fast tap-heavy flows. Appium is better for cross-platform QA infrastructure and WebDriver-based test suites.

### Can I run this in CI?

Yes, as long as your CI runner can access an Android emulator or connected device with `adb`. The commands are shell-friendly and return normal exit codes.

## Related guides

- [Fast Android UI Automation with ADB](2026-05-25-fast-android-ui-automation-with-adb.md)
- [How to Tap Android Buttons by Text from the Command Line](2026-05-25-tap-android-buttons-by-text-command-line.md)
- [Handsets vs Appium](2026-05-24-handsets-vs-appium.md)
- [Best Appium Alternative for Android Automation](2026-05-24-best-appium-alternative-for-android-automation.md)
- [How to Automate Android Apps Without Root](2026-05-24-how-to-automate-android-apps-without-root.md)
