---
date: 2026-05-22
slug: android-ui-dump-for-llms
description: A compact Android UI dump format for LLM agents that cuts XML token usage by roughly 8-13x while preserving actionable UI labels.
categories:
  - LLM agents
  - Design
---

# An Android UI dump for LLMs (10× fewer tokens, same actions)

When an LLM agent drives an Android device, the loop looks like:

1. Take a screenshot (or screen description)
2. Decide what to tap
3. Tap it
4. Repeat

For step 1, the canonical answer is `uiautomator dump` — the XML
hierarchy that `uiautomator2`, Appium, and most Android automation
tools return. On the Settings home screen of an emulator I just
measured, that XML is **22.3 KB / 5,762 GPT-4 tokens**.

The same screen rendered through Handsets' `hs ui -i` is **3.3 KB /
729 tokens** — about **8× fewer tokens, and 10–13× on simpler screens**.
The agent's decision quality doesn't change.

Here's how we got there, and why every byte that disappeared is a byte
the LLM didn't need.

---

## Three screens, two formats

| screen          | `uiautomator dump` (XML)   | `hs ui -i`            | ratio  |
| --------------- | -------------------------: | --------------------: | -----: |
| Launcher home   | 12.0 KB / **3,153 tok**    |  1.1 KB / **246 tok** | 12.8×  |
| Settings home   | 22.3 KB / **5,762 tok**    |  3.3 KB / **729 tok** |  7.9×  |
| Settings → Apps | 15.2 KB / **4,050 tok**    |  0.9 KB / **320 tok** | 12.7×  |

Token counts are from `tiktoken` with the GPT-4 encoding; reproducer at
the bottom. The ratio is bigger on screens where the layout tree is
deeper than the labeled content, and smaller on screens like Settings
home where almost every label is a real `TextView` with a real id.

A typical agent loop step now carries ~1k tokens of UI dump instead of
~5k. Across a 50-step trajectory that's an order of magnitude less
context per loop — which is real money once you're paying per token.

## The XML you start with

The first ~1.2 KB of `uiautomator dump` from the launcher home — which
covers exactly the **outer three layout nodes** of the tree:

```xml
<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>
<hierarchy rotation="0">
  <node index="0" text="" resource-id="" class="android.widget.FrameLayout"
        package="com.google.android.apps.nexuslauncher" content-desc=""
        checkable="false" checked="false" clickable="false" enabled="true"
        focusable="false" focused="false" scrollable="false"
        long-clickable="false" password="false" selected="false"
        bounds="[0,0][1440,3120]">
    <node index="0" text="" resource-id="" class="android.widget.LinearLayout"
          package="com.google.android.apps.nexuslauncher" content-desc=""
          checkable="false" ... >
      <node index="0" text="" resource-id="android:id/content"
            class="android.widget.FrameLayout" ... >
```

These three nodes are 100% noise for an agent. They have no text, no
content description, no interactivity, no clickable affordance. They
exist because Android renders surfaces by nesting `FrameLayout` inside
`LinearLayout` inside `FrameLayout`. A `clickable="false"` attribute is
a string the LLM has to *read* in order to learn that nothing is
happening here.

The rest of the dump is the same pattern, deeper. By the time the XML
reaches an actual tappable widget — say, a `TextView` with
`text="Phone"` — it has accumulated a dozen ancestors and about 600
bytes of structural padding.

## The flat table you want

Here is `hs ui -i` for the **entire launcher home screen**:

```
@(720,383)    long        ViewPager #smartspace_card_pager       desc="At a glance"
@(279,374)    click       TextView #date                         "Fri, May 22"
@(555,2063)   click,long  TextView                               "Gmail"
@(884,2063)   click,long  TextView                               "Photos"
@(1213,2063)  click,long  TextView                               "YouTube"
@(720,1590)               View                                   desc="Home"
@(226,2546)   click,long  TextView                               "Phone"
@(555,2546)   click,long  TextView                               "Messages"
@(884,2546)   click,long  TextView                               "Chrome"
@(1213,2546)  click,long  TextView                               "YouTube"
@(720,2862)   click,long  FrameLayout #search_container_hotseat  desc="Google search"
@(218,2862)   click       ImageView #g_icon                      desc="Google app"
@(1054,2862)  click       ImageView #mic_icon                    desc="Voice search"
@(1222,2862)  click       ImageButton #lens_icon                 desc="Google Lens"
```

Fourteen lines, 246 tokens. Every line is a thing the agent can decide
about. Every line has a coordinate to feed to `tap`, the action tags,
and the label to match against. No closing tags, no namespace prefixes,
no attributes whose value is `"false"`.

The four columns, left to right:

1. **Center coordinates** — `@(x,y)`. What you tap. Not the bounds
   rectangle.
2. **Behavior tags** — `click`, `long`, `scroll`, `check`, `checked`,
   `password`. What this widget responds to. Only the positive flags
   appear.
3. **Class + id** — short forms. `android.widget.Button` collapses to
   `Button`; `com.android.settings:id/title` collapses to `#title`.
4. **Label** — `"text"` or `desc="content-description"`. The
   accessibility-curated string a human (and the LLM) actually reads.

## What we threw away

Six categories of node and attribute disappeared between the XML and
the table.

**1. Empty layout containers.**
A `FrameLayout` / `LinearLayout` / `ConstraintLayout` with no text, no
content-description, and no `clickable`/`scrollable` flag is a
structural artifact of Android's renderer. Children carry the labels;
the parent's `onClick` (if any) bubbles up when you tap a child's
coords. We drop the entire subtree of layout ancestors.

**2. Attributes whose value is the default.**
`checkable="false"`, `enabled="true"`, `focused="false"`. XML serialises
every attribute regardless of value, so a `<node>` with one interesting
property still carries 14 strings of negation. We only emit positive
flags, and only the ones that change behavior.

**3. Bounds rectangles.**
`bounds="[180,810][900,910]"` is four numbers. The agent never needs
the rectangle — it taps a point. We compute the center once and store
two numbers.

**4. Class fully-qualified names.**
`android.widget.Button` → `Button`. The package prefix is informational;
the leaf is the type.

**5. Long id paths.**
`com.android.settings:id/dashboard_tile_pref_title` →
`#dashboard_tile_pref_title`. The package and namespace separator are
always the same, so they're free to drop.

**6. Decorative `View` nodes with no labels.**
A bare `<node class="View" />` with no text, no desc, no flags appears
in many Material layouts as a divider or background. They don't accept
input and they don't have anything to read.

The rule across all six: **drop fields the LLM cannot act on, and drop
default values that the format makes you write anyway.**

## What we kept that surprises people

**Inherent input widgets, even when empty.**
An `EditText` with `text=""` and `desc=""` carries no information for a
*reader*, but for an *agent* it's a target — "this is where I'd type my
email." So `EditText`, `Button`, `Switch`, `CheckBox`, `Spinner`,
`SeekBar`, `WebView`, and friends always show up, labeled or not. The
filter (from
[`ui_dump.rs`](https://github.com/elliotgao2/handsets/blob/main/handsets-cli/src/ui_dump.rs))
is:

```rust
fn is_interactive(node) -> bool {
    if !text.is_empty() || !desc.is_empty() { return true; }
    matches!(class_short,
        "EditText" | "Button" | "ImageButton" | "Switch" | "CheckBox"
        | "RadioButton" | "ToggleButton" | "Spinner" | "SeekBar"
        | "RatingBar" | "WebView"
        | "AutoCompleteTextView" | "MultiAutoCompleteTextView"
        | "DatePicker" | "TimePicker" | "NumberPicker")
}
```

If you forget about empty inputs, the agent can dump the screen and
report "no email field here" when in fact the field is sitting there
waiting to be focused. We learned that the hard way.

## Why a table, not JSON

Both are valid serialisations of the same data. JSON has the advantage
of structure that parsers expect; tables have the advantage that they
tokenize as well as JSON without paying for the structural overhead.
For the same Settings home screen:

| variant            | bytes  | tokens |
| ------------------ | -----: | -----: |
| `hs ui --json`     | 20,777 | 5,353  |
| `hs ui -i` (table) |  3,339 |   729  |

A 7× difference, almost entirely from JSON's per-row repetition:
`{ "coords": [...], "tags": [...], "class": "...", "id": "...", "text": "..." }`.
Tokenizers handle column-aligned text very efficiently — the column
header is implicit, so the per-row cost is just the values.

For tool output that an LLM *reads* one screen at a time — i.e. you
don't need a parser, you need the model to *understand* it — tables
beat JSON. (For programmatic consumption by other code, `hs ui --json`
is right there.)

## Generalising the lesson

If you build any tool that feeds an LLM, the playbook is roughly:

1. **Drop fields the model can't act on.** Anything the LLM reads but
   never references in its reply is pure tax. Grep the model's actual
   responses for which keys it cites; the rest is removable.
2. **Drop default values.** A serialisation that emits `enabled="true"`
   on every node is paying for a non-decision on every node.
3. **Pre-compute the thing the model would compute anyway.** Center
   coords instead of bounds rectangles. Short names instead of FQNs.
   Behavior tags instead of nine boolean attributes.
4. **Prefer tabular to nested when the data is regular.** Structural
   overhead of JSON / XML compresses badly through a tokenizer.
5. **Keep labels first-class.** A user-facing string ("Continue", "Sign
   in") is what the model picks. Make it the cheapest column to read.

Most of this isn't Android-specific. It applies to any "give the LLM
the state of the world" tool — search result lists, database rows,
filesystem trees. The savings compound: at 10× fewer tokens, you fit
10× more screens of trajectory into the context, or pay 10× less per
loop step.

## Caveats

- `hs ui -i` is for *action selection*, not forensic UI debugging. If
  you need every node — including layout containers, because you're
  diagnosing why a button isn't tappable — use `hs ui --xml` or
  `hs ui --xml --all`.
- The filter is conservative: a non-clickable `TextView` with text
  still shows up, because labels next to the actual button are how the
  model often refers to it ("the field under 'Email'"). Dropping
  label-only nodes shrinks the dump further but breaks selectors like
  `hs find 'EditText:below(TextView[text=Email])'`.
- Filing one bug report against an LLM agent loop usually requires the
  full XML at the moment of failure. `hs ui --xml` exists for exactly
  that.
- `uiautomator dump` and the Handsets daemon both hold the system's
  `UiAutomation` connection exclusively, so to capture both formats of
  the same screen you have to `hs drop`, run `uiautomator dump`, then
  `hs use` again. This is a real annoyance, not a benchmark gotcha.

## Reproducing

```bash
# Set up
curl -fsSL https://raw.githubusercontent.com/elliotgao2/handsets/main/install.sh | bash
hs use

# Canonical XML — the hs daemon must release UiAutomation first
hs drop
adb shell uiautomator dump /sdcard/u.xml && adb pull /sdcard/u.xml /tmp/u.xml

# hs format
hs use
hs ui -i > /tmp/hs.txt

# Bytes
wc -c /tmp/u.xml /tmp/hs.txt

# Tokens (pip install tiktoken)
python3 - <<'PY'
import tiktoken
enc = tiktoken.encoding_for_model("gpt-4")
for p in ("/tmp/u.xml", "/tmp/hs.txt"):
    t = open(p).read()
    print(f"{p}: {len(t):>8d} bytes  {len(enc.encode(t)):>6d} tokens")
PY
```

If your screen produces a materially different ratio, I'd be curious
to see it. An app with a deep custom layout tree should produce a
*bigger* ratio, not smaller; if it's smaller, the filter probably has
a hole.

## Related guides

- [How to Automate Android Apps Without Root](2026-05-24-how-to-automate-android-apps-without-root.md)
- [How to Control an Android Phone Without Root](2026-05-24-how-to-control-android-phone-without-root.md)
- [Tapping Android in 5 ms](2026-05-23-tapping-android-in-5ms-vs-appium-uiautomator2.md)

---

*[Handsets](https://github.com/elliotgao2/handsets) is a CLI for
driving Android devices, built for LLM agents and shell scripts.
`hs ui -i` is one verb; the rest are designed under the same
constraint. MIT.*
