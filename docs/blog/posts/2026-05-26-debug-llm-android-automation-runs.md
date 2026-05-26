---
date: 2026-05-26
slug: debug-llm-android-automation-runs
description: "How to debug LLM-driven Android automation runs with action timelines, UI dumps, screenshots, logs, structured errors, and replayable traces."
categories:
  - LLM agents
  - Android automation
  - Debugging
---

# How to Debug LLM-Driven Android Automation Runs

LLM-driven Android automation fails in strange ways.

The model may tap the wrong label. The screen may change between observation and action. A keyboard may cover the button. A permission dialog may appear. The app may still be loading. The UI dump may expose two identical "Continue" buttons.

If all you saved is the final screenshot, debugging is painful.

You need a run trace.

<!-- more -->

## Quick answer

For every Android agent step, save:

- the compact UI dump
- the screenshot when needed
- the model's chosen action
- the actual device command
- the result or structured error
- recent logs
- the top package/activity

The minimum useful trace looks like this:

```text
observe: tap Button "Continue" #continue 540,860
model:   tap "Continue"
action:  hs tap "Continue" --visible --unique
result:  ok
wait:    hs wait "Dashboard" --timeout 15s
result:  TIMEOUT
```

That is much easier to debug than "the agent failed."

## The failure modes

Android agent failures usually fall into a few buckets.

| Failure | What it means |
| --- | --- |
| `NOT_FOUND` | The target label or selector was not visible |
| `AMBIGUOUS` | More than one visible node matched |
| `TIMEOUT` | The expected next state never appeared |
| `SECURE_WINDOW` | Android blocked screenshots for the current window |
| Wrong action | The model chose a bad label or command |
| Stale observation | The UI changed after the model saw it |

Good tooling should preserve which bucket happened.

If everything becomes "click failed", the agent cannot recover intelligently.

## Save the UI dump before the action

The UI dump is the agent's view of the world.

Save it before each model decision:

```bash
hs ui > run/0007-ui.txt
```

For LLM agents, a compact action table is usually better than full XML:

```text
fill  EditText  "Email"     #email     540,540
fill  EditText  "Password"  #password  540,640  [password]
tap   Button    "Continue"  #continue  540,860
```

When a model picks the wrong action, this file tells you whether the model had a reasonable choice.

## Save screenshots selectively

Screenshots are valuable, but you do not need a full native PNG on every step.

For most agent debugging:

```bash
hs see --size 768 run/0007-screen.jpg
```

Use screenshots when:

- the UI dump has too little information
- the app renders custom controls
- visual layout matters
- a failure needs human review

Use the text UI as the default. Use screenshots as evidence.

## Record the model action separately

Do not only save the final command.

Save what the model actually emitted:

```json
{
  "step": 7,
  "model_action": "tap \"Continue\"",
  "tool_call": ["hs", "tap", "Continue", "--visible", "--unique"],
  "reason": "The login form is filled and Continue is visible."
}
```

This matters because the bug may be in translation:

- The model chose the right label, but the tool call used the wrong selector.
- The model chose a coordinate when a label was available.
- The model ignored an ambiguity warning.

Keep the model layer and tool layer separate.

## Prefer structured errors

Exit codes and error codes are better than stderr scraping.

Handsets has common exit codes:

```text
0  ok
2  NOT_FOUND
3  TIMEOUT
4  AMBIGUOUS
```

In JSON mode, preserve the structured error:

```bash
hs --json tap "Continue" --visible --unique
```

Then your agent can decide:

- `NOT_FOUND`: dump UI again or scroll
- `AMBIGUOUS`: ask for a narrower selector
- `TIMEOUT`: capture screenshot and logs
- `SECURE_WINDOW`: continue without screenshot

## Keep logs close to the failing step

Android logs are noisy. A small tail near the failure is usually enough:

```bash
hs logs --tail 200 > run/0007-logcat.txt
```

Pair logs with the UI dump and screenshot from the same step. Otherwise you end up with artifacts that are technically present but hard to correlate.

## A simple artifact layout

Use numbered files:

```text
run/
  0001-ui.txt
  0001-action.json
  0001-result.json
  0002-ui.txt
  0002-screen.jpg
  0002-action.json
  0002-result.json
  0002-logcat.txt
```

This is not fancy. That is the point.

Before building a dashboard, make the run inspectable with plain files.

## Replay is the next step

Once you have traces, replay becomes possible.

The useful replay is not pixel-perfect video. It is a timeline:

```text
Step 1: observed Sign in
Step 2: tapped Sign in
Step 3: filled Email
Step 4: filled Password
Step 5: tapped Continue
Step 6: timed out waiting for Dashboard
```

For teams, this timeline becomes the product. It lets an engineer see whether the model, the tool, or the app caused the failure.

## FAQ

### Why are LLM Android agents hard to debug?

Because failures can come from the model, the app, the Android UI state, the automation tool, or timing. A final screenshot does not tell you which layer failed.

### Should I save screenshots for every step?

Not always. Save compact UI dumps for every step. Add screenshots for visual states, failures, and custom-rendered screens.

### What is the most important artifact?

The pre-action UI dump. It shows what the model saw when it chose the action.

### How does this help reliability?

Structured traces let you build targeted recovery: scroll on `NOT_FOUND`, narrow selectors on `AMBIGUOUS`, capture logs on `TIMEOUT`, and avoid retrying blindly.

## Related guides

- [Android Automation for LLM Agents](2026-05-25-android-automation-for-llm-agents.md)
- [Android Device Cloud for LLM Agents](2026-05-26-android-device-cloud-for-llm-agents.md)
- [Stop Wasting Tokens on Android Automation](2026-05-24-stop-wasting-tokens-on-android-automation.md)
