---
date: 2026-05-24
slug: stop-wasting-tokens-on-android-automation
description: LLM-driven Android automation can waste thousands of tokens per step on screenshots and XML trees; here is how Handsets cuts that down to the actionable UI.
categories:
  - LLM agents
  - Android automation
  - Performance
---

# Stop Wasting Tokens on Android Automation

Most LLM-driven Android automation starts by showing the model a screen.

That sounds reasonable. A human looks at the phone, decides what to tap, and taps it. Give the model the same view.

The problem is that "the same view" is expensive.

A full screenshot is expensive. A raw Android UI XML dump is also expensive, just in a quieter way. The model reads thousands of tokens of layout machinery before it reaches the handful of labels that matter:

```text
Email
Password
Continue
```

For one step, that waste is easy to ignore. For a 50-step mobile agent trajectory, it becomes the bill.

<!-- more -->

## The loop

An Android agent usually does this:

1. Read the current screen.
2. Decide what to do.
3. Tap, type, or swipe.
4. Wait for the next screen.
5. Repeat.

The first step is where the token leak begins.

If you use `uiautomator dump`, the model gets XML like this:

```xml
<node index="0" text="" resource-id=""
      class="android.widget.FrameLayout"
      package="com.google.android.apps.nexuslauncher"
      content-desc=""
      checkable="false" checked="false"
      clickable="false" enabled="true"
      focusable="false" focused="false"
      scrollable="false" long-clickable="false"
      password="false" selected="false"
      bounds="[0,0][1440,3120]">
```

That is one layout node. It says almost nothing an agent can act on.

It is not a bug in UIAutomator. XML is a faithful serialization of the accessibility tree. Faithful is not the same as useful.

## The numbers

On a few ordinary Android screens, the difference looks like this:

| screen | UIAutomator XML | Handsets `hs ui -i` | reduction |
| --- | ---: | ---: | ---: |
| Launcher home | 3,153 tokens | 246 tokens | 12.8x |
| Settings home | 5,762 tokens | 729 tokens | 7.9x |
| Settings -> Apps | 4,050 tokens | 320 tokens | 12.7x |

Token counts are from `tiktoken` with the GPT-4 encoding. The deeper write-up is [An Android UI Dump for LLMs](2026-05-22-android-ui-dump-for-llms.md).

The short version: a typical screen that costs 4,000-6,000 tokens as XML can often be represented in a few hundred tokens as an action table.

Across 50 steps, that is the difference between sending roughly 250k tokens of screen state and sending roughly 25k-40k.

The agent usually makes the same decision either way.

## What the model actually needs

For UI automation, the model does not need a DOM-shaped tree.

It needs a list of things it can act on:

```text
fill  EditText  "Email"     #email     540,540
fill  EditText  "Password"  #password  540,640  [password]
tap   Button    "Continue"  #continue  540,860
```

That table gives the model the useful facts:

- What action is available.
- What label a human sees.
- What type of control it is.
- Where the tool will tap or type.

The model can now answer:

```text
tap "Continue"
```

It does not have to parse layout ancestors, negative booleans, fully-qualified class names, or four-number bounds rectangles.

## The rule

For LLM tool output, the optimization rule is simple:

> Do not serialize facts the model cannot use in its next action.

Android XML violates that rule constantly:

- `clickable="false"` on nodes the agent will never click.
- `enabled="true"` repeated on almost every node.
- Empty `FrameLayout` and `LinearLayout` containers.
- Full class names like `android.widget.TextView`.
- Bounds rectangles when the agent only needs a tap point.
- JSON-style key repetition when the reader is a language model, not a parser.

Handsets drops the defaults, shortens the names, computes the center point, and keeps the labels.

The result is not a smaller XML file. It is a different interface:

```bash
hs ui
hs tap "Continue"
hs wait "Dashboard"
```

## Screenshots are still useful

This is not an argument against screenshots.

Screenshots are useful when layout matters, when visual state matters, or when an app renders important information without accessible labels.

But screenshots are a poor default for every step. They are large, slow to move, and often force the model to do OCR-like work for text that Android already exposes.

A better loop is:

```bash
hs ui > /tmp/screen.txt
hs see --size 768 /tmp/screen.jpg   # only when visual context matters
```

Give the model the text UI first. Add the image when the text is not enough.

That usually saves tokens and makes the action easier to audit.

## Why this matters more for agents than tests

Traditional mobile tests do not care much about token count. A test runner is not paying to read XML.

LLM agents are different. Every loop step has a context budget and a cost. If half the prompt is a UI tree full of dead layout nodes, the model is spending attention on junk.

This shows up in three places:

- **Cost:** repeated screen state dominates long trajectories.
- **Latency:** large prompts take longer to send and process.
- **Reliability:** shorter action-oriented context leaves less room for the model to latch onto irrelevant structure.

The best tool output for an agent is not the most complete representation of the system. It is the smallest representation that preserves the next correct action.

## The practical pattern

For Android, the pattern looks like this:

```bash
hs use
hs ui
hs tap "Sign in"
hs fill "Email" "you@example.com"
hs fill "Password" "$PASSWORD"
hs tap "Continue"
hs wait "Dashboard"
```

For an LLM, the important handoff is even smaller:

```text
Here is the current Android UI. Pick the next action by label.

fill  EditText  "Email"     #email     540,540
fill  EditText  "Password"  #password  540,640  [password]
tap   Button    "Continue"  #continue  540,860
```

The model does not need to know that these nodes live inside three nested `FrameLayout`s. It needs to know that "Continue" is a button.

## Related guides

- [Android Device Cloud for LLM Agents](2026-05-26-android-device-cloud-for-llm-agents.md)
- [How to Debug LLM-Driven Android Automation Runs](2026-05-26-debug-llm-android-automation-runs.md)
- [Android Automation for LLM Agents](2026-05-25-android-automation-for-llm-agents.md)
- [uiautomator2 Alternative for Android Automation](2026-05-24-uiautomator2-alternative-for-android-automation.md)
- [An Android UI Dump for LLMs](2026-05-22-android-ui-dump-for-llms.md)
- [Tapping Android in 5 ms](2026-05-23-tapping-android-in-5ms-vs-appium-uiautomator2.md)
- [How to Automate Android Apps Without Root](2026-05-24-how-to-automate-android-apps-without-root.md)
