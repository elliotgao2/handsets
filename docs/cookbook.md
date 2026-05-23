# Cookbook

*Recipes for driving Android with `hs`. Each one is copy-pasteable into a
real terminal.*

These all build on the agent loop in the [README](../README.md) and the
verb table in `hs --help`. If you've never run `hs use`, start there.
The shared action flags (`--timeout`, `--retries`, `--visible`,
`--unique`, `--nth`, `--json`, …) work on every action verb; this page
shows them in context rather than restating the table.

---

## Logging in

The canonical flow: find the email field, fill it, find the password
field, fill it, submit, and wait for the dashboard.

```bash
hs use
hs wait idle 200ms

hs tap   "Sign in"              --visible --unique --timeout 5s
hs fill  [resource-id=com.example:id/email]    "you@example.com"
hs fill  [resource-id=com.example:id/password] "hunter2"
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
launches.

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
      hs see /tmp/hung.jpg ;;
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

Or as one batched script over a single warm socket:

```bash
hs run - <<'EOF'
set timeout=500ms
set retries=12
swipe up 250
tap "Settings" --visible --unique
EOF
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
moment a verb failed:

```bash
set -e
trap 'hs see /tmp/handsets-$$-failure.jpg' ERR

hs tap  "Sign in" --unique
hs wait "Dashboard" --timeout 10s
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

## "Do then verify" in one call

Tap a button and only succeed if the next screen actually appears — no
more `tap ... && sleep 0.5 && wait ...` chains:

```bash
hs act --tap   "Send"        --until "Sent"            --timeout 8s
hs act --fill  [id=q] "claude" --until 'RecyclerView'  --timeout 5s
hs act --swipe up 300        --until-idle              --timeout 2s
```

Exit 0 only if both halves succeeded; `TIMEOUT` if the predicate never
fired; `NOT_FOUND` if the action itself couldn't execute.

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
echo 'tap "Refresh"; wait "Synced"' | hs run -
```
