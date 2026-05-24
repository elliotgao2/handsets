---
date: 2026-05-24
slug: how-to-automate-android-without-appium
description: "Automate Android without Appium using a lightweight CLI: tap by text, fill fields, wait for screens, and run no-root scripts from your terminal."
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

## Related guides

- [Handsets vs Appium](2026-05-24-handsets-vs-appium.md)
- [Best Appium Alternative for Android Automation](2026-05-24-best-appium-alternative-for-android-automation.md)
- [How to Automate Android Apps Without Root](2026-05-24-how-to-automate-android-apps-without-root.md)
