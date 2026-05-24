---
date: 2026-05-24
slug: how-to-take-screenshots-on-android-without-root
description: Take Android screenshots from the command line without root, with fast JPEG captures for agents and native exports for debugging.
categories:
  - Screenshots
  - No root
---

# How to Take Screenshots on Android Without Root

You can take screenshots from an Android phone without root.

Android already allows the shell user to capture normal, non-secure windows through the system screenshot path. The catch is that the usual command is not very friendly:

```bash
adb exec-out screencap -p > screen.png
```

It works. It is also slow, always PNG, and awkward when you are taking screenshots inside a loop.

Handsets gives you a smaller command:

```bash
hs see screen.jpg
```

No root. No installed app. Just `adb`.

<!-- more -->

## Install and connect

First, make sure the device appears in `adb devices`.

Then install Handsets:

```bash
curl -fsSL https://raw.githubusercontent.com/elliotgao2/handsets/main/install.sh | bash
```

Start the device daemon:

```bash
hs use
```

Now capture a screenshot:

```bash
hs see screen.jpg
```

The file extension chooses the format. Use `.jpg` for normal automation, `.png` when you need lossless debugging, and `.webp` when you want compact lossy output.

## Fast screenshots for agents

If a screenshot is going into an LLM, you usually do not need the full native resolution. A 1440x3120 phone screenshot is expensive to move, encode, and send to a model.

Use an agent-sized image:

```bash
hs see --size 768 /tmp/screen.jpg
```

That keeps the long edge at 768 pixels. It is enough for layout decisions, button placement, and visual context. It is also much faster than pushing a full PNG through `adb exec-out`.

For many agent loops, you should pair the screenshot with a text UI dump:

```bash
hs ui > /tmp/ui.txt
hs see --size 768 /tmp/screen.jpg
```

The text dump tells the model what can be tapped. The image helps when layout or visual state matters.

## Native screenshots for debugging

When you are filing a bug or checking pixels, ask for the native export:

```bash
hs see --native /tmp/full.jpg
hs see --native /tmp/full.png
```

Use native captures when detail matters. Use smaller captures when speed matters.

That split is simple, but it saves a lot of time. Most automation does not need a full-resolution PNG on every step.

## What can block a screenshot

No-root screenshot capture still follows Android's rules.

If the foreground app marks a window with `FLAG_SECURE`, Android blocks screenshots. Banking apps, password managers, streaming apps, and some login screens do this intentionally.

That is not a Handsets limitation. It is the platform doing what the app requested.

When that happens, a no-root tool should fail clearly. It should not pretend to see the screen. It should not bypass app security.

## A practical capture loop

Here is a small loop that captures the UI text and a screenshot whenever a step fails:

```bash
hs tap "Submit" --visible --unique --timeout 5s
case $? in
  0)
    echo "submitted"
    ;;
  2|3|4)
    hs ui > /tmp/failed-ui.txt || true
    hs see --size 768 /tmp/failed-screen.jpg || true
    exit 1
    ;;
esac
```

That is usually enough for unattended jobs. You get the screen, the tappable labels, and the exact failure point.

## The short version

Use this for normal no-root screenshots:

```bash
hs see screen.jpg
```

Use this for fast screenshots inside an automation or agent loop:

```bash
hs see --size 768 screen.jpg
```

Use this when you need full detail:

```bash
hs see --native screen.png
```

Root is not part of the story.
