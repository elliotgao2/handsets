# Cookbook

*Recipes for driving Android with `hs`. Each one is copy-pasteable into a
real terminal.*

These build on the agent loop in the [README](index.md) and the verb
table in `hs --help`. If you've never run `hs use`, start there. The
shared action flags (`--timeout`, `--retries`, `--visible`, `--unique`,
`--nth`, `--json`, …) work on every action verb; this page shows them in
context rather than restating the table.

---

## Fast agent loop

Use the UI table as the primary model input. It is small, text-first, and
already contains the center coordinates Handsets will tap.

```bash
hs use
hs ui > /tmp/hs-screen.txt
```

Only add an image when the visual layout matters. Keep it agent-sized:

```bash
hs see --size 768 /tmp/hs-screen.jpg
```

`hs see out.jpg` without `--size` is a native-resolution export for bug
reports and pixel debugging. It is slower and much larger than the 768px
agent path. PNG is lossless/debug only:

```bash
hs see --native /tmp/full.jpg
hs see --native /tmp/full.png
```

## Logging in

The canonical flow: find the email field, fill it, find the password
field, fill it, submit, and wait for the dashboard.

```bash
hs use
hs wait idle 200ms

hs tap   "Sign in"              --visible --unique --timeout 5s
hs fill  'EditText[resource-id=com.example:id/email]'    "you@example.com"
hs fill  'EditText[resource-id=com.example:id/password]' "hunter2"
hs submit
hs wait  "Dashboard"            --timeout 15s
```

If the app has two "Sign in" buttons (header and footer, for example),
`--unique` will fail with exit 4 (`AMBIGUOUS`); pick the one you want
with `--nth 1` or tighten the selector. When IDs aren't stable, lean on
the relational pseudo-classes:

```bash
hs fill 'EditText:below(TextView[text=Email])'    "you@example.com"
hs fill 'EditText:below(TextView[text=Password])' "hunter2"
```

If the field already has focus and you want the real keyboard path rather
than atomic set-text, use `type`:

```bash
hs type "you@example.com"
hs submit
```

## Waiting for the next screen

`hs wait` is overloaded by argument shape so it stays readable:

```bash
hs wait idle 200ms        # block until the UI settles for 200 ms
hs wait "Welcome"         # block until that text appears anywhere
hs wait com.foo           # block until that package is in the foreground
hs wait com.foo/.MainAct  # block on a specific activity
hs wait 500ms             # client-side sleep (no daemon hop)
```

All forms honour `--timeout`. The default is 10 s — bump it for slow
launches. Prefer these waits over shell `sleep`; a fixed sleep is both
slower on fast devices and flaky on slow ones.

## Do then verify in one call

Tap a button and only succeed if the expected next state appears. This is
the usual replacement for `tap && sleep && wait` chains:

```bash
hs act --tap   "Send"          --until "Sent"             --timeout 8s
hs act --fill  '[id=q]' "llm"  --until-selector RecyclerView --timeout 5s
hs act --swipe up 300          --until-idle               --timeout 2s
```

Exit 0 only if both halves succeeded. `TIMEOUT` means the predicate never
appeared; `NOT_FOUND` means the action itself could not execute.

## Retrying a flaky tap

Daemon-side waits get retried automatically when a `TIMEOUT` comes back,
so the script stays linear:

```bash
hs tap  "Refresh" --retries 4 --retry-delay 500ms --timeout 3s
hs wait "Synced"  --retries 4 --timeout 5s
```

For unattended jobs, branch on the exit code instead of parsing stderr.
There are only three codes worth a `case` arm:

```bash
hs tap "Submit" --unique --timeout 5s
case $? in
  0)  echo "submitted" ;;
  2)  echo "no Submit button on screen" ;;        # NOT_FOUND
  3)  echo "tap acked but UI hung — escalating"   # TIMEOUT
      hs see --size 768 /tmp/hung.jpg ;;
  4)  echo "ambiguous — narrow the selector" ;;   # AMBIGUOUS
  *)  echo "unexpected failure"; exit 1 ;;
esac
```

The full structured `ErrCode` (`DAEMON_ERROR`, `PRECONDITION`,
`SECURE_WINDOW`, …) is preserved in `--json` output as `error.code` —
use that when you need to dispatch on the long tail.

## Scrolling until something is visible

A 12-attempt scroll-then-tap loop in five lines:

```bash
for _ in $(seq 1 12); do
  hs tap "Settings" --visible --unique --timeout 500ms && break
  hs swipe up 250
done
```

If you already know one scroll is enough, batch the scroll and tap over a
single warm socket:

```bash
hs run - <<'EOF'
set timeout=500ms
swipe up 250
tap "Settings" --visible --unique
EOF
```

`hs run` executes one command per line. Do not put multiple verbs on one
line with semicolons. Use the shell `for` loop above when you need
conditional looping.

## Debugging selectors

Start broad, inspect the JSON, then tighten. This is faster than guessing
at coordinates.

```bash
hs find 'Button:has-text("Continue")' --json | jq .
hs find '*[text=Continue], *[text=Next]' --visible --json | jq .
```

If there are multiple matches, either make the selector structural or pick
an index explicitly:

```bash
hs tap 'Button:has-text("Continue"):in(LinearLayout[id~=footer])' --unique
hs tap 'Button:has-text("Continue")' --visible --nth 2
```

## Two-factor SMS

`hs sms` reads the inbox directly via `IContentProvider` — no installed
helper app, no permission grants.

```bash
hs wait "Enter the code"
CODE=$(hs sms inbox --limit 1 --json | jq -r '.[0].body' | grep -oE '[0-9]{6}')
hs type   "$CODE"
hs submit
```

For apps that put the code on the clipboard, read or paste it directly:

```bash
CODE=$(hs clip)
hs type "$CODE"
hs paste 'EditText:focused'
```

Clipboard watch is useful for magic-link and OTP handoff scripts:

```bash
hs clip --watch --interval 250
```

## Notifications and push flows

Trigger a server-side push, then read recent notifications. Filter by
package when you know it:

```bash
hs notif com.example.app --limit 5 --json | jq .
hs notif --history --limit 20 --json | jq .
```

For login links that open a browser or app, wait on the resulting package
or activity instead of sleeping:

```bash
hs wait com.example.app --timeout 15s
hs wait com.example.app/.MainActivity --timeout 15s
```

## Running against many devices

`hs fan` runs the same verb against each serial in parallel and exits
non-zero if any device fails:

```bash
hs fan PIXEL6,PIXEL7,EMU -- tap "Continue" --visible --timeout 5s
```

For machine-readable per-device output, add the global `--json`:

```bash
hs --json fan PIXEL6,EMU -- wait "Welcome" --timeout 10s | jq -c
```

## Screenshotting on failure

A one-line `bash -e` trap that captures whatever was on screen the
moment a verb failed. Use the fast 768px JPEG path for unattended runs:

```bash
set -e
trap 'hs see --size 768 /tmp/handsets-$$-failure.jpg' ERR

hs tap  "Sign in" --unique
hs wait "Dashboard" --timeout 10s
```

When you need a full artifact bundle for CI, collect the UI, device state,
logs, and screenshot together:

```bash
DIR=/tmp/handsets-failure-$$
mkdir -p "$DIR"
trap 'hs ui --json > "$DIR/ui.json"; hs ui --xml > "$DIR/ui.xml"; hs info > "$DIR/info.txt"; hs logs --tail 200 > "$DIR/logs.txt"; hs see --size 768 "$DIR/screen.jpg"; echo "$DIR"' ERR
```

If you expect a black screenshot from a banking, password, or incognito
screen, opt into the slower FLAG_SECURE check:

```bash
hs see --secure-check --size 768 /tmp/secure-check.jpg
```

## Selectors when text alone isn't enough

Real Android UIs rarely give you stable resource IDs. Lean on the
relational pseudo-classes:

```bash
# The OK button inside a specific dialog:
hs tap '*[text="OK"]:in(LinearLayout[id~=dialog])' --unique

# The EditText below the "Email" label, visible only:
hs tap 'EditText:below(TextView[text=Email]):visible' --unique

# Buttons near a specific icon:
hs find 'Button:near(ImageView[desc~=cart], 200)'

# OR groups — "Continue" or "Next", whichever shipped this build:
hs tap '*[text=Continue], *[text=Next]' --visible --unique --timeout 5s

# Playwright-style sugar:
hs tap 'Button:has-text("Sign in")'
hs tap 'Button:text-is("Sign in")'
```

## Driving from Python

There's a first-party SDK that wraps the CLI as a context manager and
maps the exit codes to typed exceptions:

```python
# pip install handsets
from handsets import Session, NotFound, Timeout

with Session() as d:
    d.tap("Continue", visible=True, unique=True, timeout="5s")
    d.fill("[resource-id=com.example:id/email]", "you@example.com")
    d.submit()
    try:
        d.wait(text="Dashboard", timeout="15s")
    except Timeout:
        d.go("back")
```

For tight loops, the SDK opens a warm-socket batching context that
shares one `hs run -` subprocess across calls — per-call process
startup collapses into one startup for the whole batch:

```python
with Session() as d:
    with d.batch(timeout="5s", retries=2) as b:
        for label in labels:
            b.tap(label, visible=True)
        b.wait(text="Done")
```

For Node, Go, or anything else, drive `hs --json` as a subprocess and
parse one JSON line per call:

```python
import json, subprocess
r = subprocess.run(
    ["hs", "--json", "tap", "Sign in", "--visible", "--unique", "--timeout", "5s"],
    capture_output=True, text=True,
)
match r.returncode:
    case 0: print("tapped at", json.loads(r.stdout)["result"]["x"])
    case 2: raise RuntimeError("no Sign-in button on screen")
    case 3: raise TimeoutError("tap timed out")
```

## Batch scripts

For longer flows where the per-call socket churn matters, `hs run` reads
verb lines from a file (or stdin) and executes them over one warm
socket. The `set` directives raise defaults without touching every line:

```text
# flow.hs
set timeout=8s
set retries=2
set continue-on-error
set dump-ttl=200ms

wait idle
tap   "Continue"   --visible --unique
fill  [id=email]   "you@example.com"
fill  [id=pw]      "hunter2"
submit
wait  "Dashboard"  --timeout 15s
```

```bash
hs init my-flow.hs       # scaffold a starter file
hs run  my-flow.hs       # run it
```

For ad-hoc batching, pipe directly:

```bash
printf '%s\n' 'tap "Refresh"' 'wait "Synced"' | hs run -
```

## App lifecycle setup

Bring an app to the foreground, close it, or use raw shell commands for
setup that is not yet a first-class verb. Pipe shell commands into
`hs shell` so streamed command output is drained correctly:

```bash
hs open com.example.app/.MainActivity
hs close com.example.app
printf '%s\n' 'pm clear com.example.app' | hs shell
```

For deeplink-driven flows, inspect what the APK declares:

```bash
hs links com.example.app
hs open com.example.app/.MainActivity
```
