---
date: 2026-05-24
slug: how-to-manage-multiple-android-phones-without-root
description: Manage and automate multiple Android phones from the command line without rooting them.
categories:
  - Device farms
  - No root
---

# How to Manage Multiple Android Phones Without Root

Managing one Android phone is easy.

Managing ten is where the small annoyances become the whole job: which device is connected, which one failed, which screen is stuck, which command ran where.

You still do not need root.

For many small device farms, USB debugging plus `adb` is enough. Handsets adds a thin command layer that lets you run the same action across multiple devices and get per-device results.

<!-- more -->

## Start with serials

List devices with `adb`:

```bash
adb devices
```

You will see serials like:

```text
PIXEL6
PIXEL7
emulator-5554
```

Those serials are the names you use when you want to address devices explicitly.

## Run one command on many devices

Handsets has a `fan` command for this:

```bash
hs fan PIXEL6,PIXEL7,emulator-5554 -- tap "Continue" --visible --timeout 5s
```

That runs the same command against each device in parallel.

If any device fails, the overall command exits non-zero. That makes it useful in CI and in small lab scripts where one stuck device should fail the job.

## Wait for all devices to reach the same screen

After a push notification, install, or account setup step, you often want every phone to reach the same state:

```bash
hs fan PIXEL6,PIXEL7,emulator-5554 -- wait "Welcome" --timeout 20s
```

This is much cleaner than a loop that sleeps and hopes every device is ready.

If one phone never reaches the screen, you know which one needs attention.

## Get machine-readable output

For scripts, use JSON:

```bash
hs --json fan PIXEL6,PIXEL7,emulator-5554 -- wait "Dashboard" --timeout 20s | jq -c
```

That gives you per-device output that another program can parse. You can store it in CI logs, pipe it into a dashboard, or use it to decide which device needs a screenshot.

## Capture failure artifacts

When one phone fails, collect evidence from that phone.

For a small farm, a simple pattern is enough:

```bash
SERIALS="PIXEL6,PIXEL7,emulator-5554"

if ! hs fan "$SERIALS" -- wait "Dashboard" --timeout 20s; then
  for serial in PIXEL6 PIXEL7 emulator-5554; do
    hs -s "$serial" ui > "/tmp/$serial-ui.txt" || true
    hs -s "$serial" see --size 768 "/tmp/$serial-screen.jpg" || true
  done
  exit 1
fi
```

The exact serial-selection mechanism depends on your local setup, but the idea is stable: fan out for the common path, then collect artifacts from the devices that need inspection.

## What no-root management can do

With normal debugging access, you can:

- Install and uninstall APKs.
- Launch apps.
- Tap, type, swipe, and press keys.
- Wait for visible text or activities.
- Capture screenshots when the app allows it.
- Read UI state and recent logs.
- Run the same workflow across multiple devices.

That covers the day-to-day work for a small Android bench.

## What still needs device policy or root

Some management tasks are outside the no-root lane.

Factory resets, global policy enforcement, privileged app installs, private app data extraction, and protected system settings require stronger device ownership or root. If you need those, use Android Enterprise, a managed device policy, or a dedicated lab image.

But do not start there by default.

For UI automation, QA smoke tests, app setup, and agent workflows, normal phones are enough:

```bash
hs fan PIXEL6,PIXEL7,emulator-5554 -- tap "Continue"
hs fan PIXEL6,PIXEL7,emulator-5554 -- wait "Done"
```

That is the quiet win: the phones stay ordinary, and the workflow still scales past one device.

## Related guides

- [How to Install APKs on Android Without Root](2026-05-24-how-to-install-apks-on-android-without-root.md)
- [How to Run Mobile QA Tests Without Rooted Phones](2026-05-24-how-to-run-mobile-qa-tests-without-rooted-phones.md)
- [How to Automate Android Apps Without Root](2026-05-24-how-to-automate-android-apps-without-root.md)
