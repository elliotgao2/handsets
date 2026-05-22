# Handsets cookbook — recipes for RPA scripts

This is the "I want to do X" reference for `hs`. Every recipe is copy-pasteable.
Hand any of them to a person, an LLM, or a Makefile and it should just work.

If you are new to `hs`, start with [Quickstart](#0-quickstart-first-script).

---

## 0. Quickstart: first script

```bash
hs use                   # auto-pick the only connected device, start daemon
hs ui -i                 # see what's tappable on the current screen
hs tap "Continue"        # find by text, tap centre
hs type "you@x.com"      # type into the focused field
hs submit                # press the IME Go/Search/Done key
hs wait "Welcome"        # block until that text appears
hs drop                  # tear the daemon down (optional)
```

All ten recipes below build on these verbs plus the shared flag set:

| Flag             | Meaning                                                   |
| ---------------- | --------------------------------------------------------- |
| `--timeout MS`   | Per-call wait budget (overrides daemon's 10 s default)    |
| `--retries N`    | Extra attempts after the first (default 0)                |
| `--retry-delay MS` | Delay between attempts (default 200 ms)                 |
| `--visible`      | Only match nodes that pass `isVisibleToUser`              |
| `--clickable`    | Only match nodes the framework considers tappable         |
| `--enabled`      | Only match enabled nodes                                  |
| `--unique`       | Fail with exit 6 if more than one match remains           |
| `--nth I`        | Pick the I-th match (1-indexed)                           |
| `--json`         | Emit `{"verb":..., "ok":..., "result":...}` per line      |
| `--fresh`        | Force a re-dump (only meaningful inside `hs run`/`shell`) |

Exit codes (so RPA scripts can branch without parsing stderr):

| Code | Meaning           | Code | Meaning            |
| ---- | ----------------- | ---- | ------------------ |
| 0    | OK                | 6    | AMBIGUOUS          |
| 1    | INTERNAL/general  | 7    | PRECONDITION       |
| 2    | NOT_FOUND         | 8    | BAD_ARG            |
| 3    | TIMEOUT           | 9    | SECURE_WINDOW      |
| 4    | DAEMON_ERROR      | 10   | UNKNOWN_CMD        |
| 5    | DEVICE_GONE       | 11   | INTERNAL (daemon)  |

---

## 1. Login form

Goal: fill email + password, submit, wait for the post-login screen.

```bash
hs use
hs wait idle 200ms
hs tap "Sign in" --visible --unique --timeout 5s
hs type [resource-id=com.example:id/email] "you@example.com"
hs type [resource-id=com.example:id/password] "hunter2"
hs submit
hs wait "Dashboard" --timeout 15s
```

If your app has multiple "Sign in" buttons (header *and* footer), keep
`--unique` and pick the right one with `--nth 1` or a more specific
selector. If selectors aren't stable, fall back to `hs find` to discover
real IDs:

```bash
hs find '*:clickable :visible' | grep -i sign
```

## 2. Retry-on-flake without a shell `for` loop

Pre-batched retries — daemon-side waits get retried automatically when a
TIMEOUT comes back:

```bash
hs tap "Refresh" --retries 4 --retry-delay 500ms --timeout 3s
hs wait "Synced"  --retries 4 --timeout 5s
```

For an unattended job, branch on exit code:

```bash
hs tap "Submit" --timeout 5s --unique
case $? in
  0)  echo "submitted" ;;
  2)  echo "no Submit button on screen" ;;
  3)  echo "tap acked but UI hung — escalating"; hs see /tmp/hung.jpg ;;
  6)  echo "ambiguous Submit — narrow the selector" ;;
  *)  echo "unexpected failure"; exit 1 ;;
esac
```

## 3. Scroll until visible, then tap

```bash
for _ in $(seq 1 12); do
  hs tap "Settings" --visible --unique --timeout 500ms && break
  hs swipe up 250
done
```

Or as a single batched script over one warm socket:

```bash
hs run - <<'EOF'
set timeout=500ms
set retries=12
# The retry layer re-issues the tap until it lands.
swipe up 250
tap "Settings" --visible --unique
EOF
```

## 4. Two-factor SMS

```bash
hs wait "Enter the code"               # screen marker
CODE=$(hs sms inbox --limit 1 --json | jq -r '.[0].body' | grep -oE '[0-9]{6}')
hs type "$CODE"
hs submit
```

## 5. Multi-device fan-out

Run the same flow against three devices in parallel and collect per-device
exit codes:

```bash
hs fan PIXEL6_SERIAL,PIXEL7_SERIAL,EMU_SERIAL -- tap "Continue" --visible --timeout 5s
echo "fan exit: $?"   # non-zero if any device failed
```

For machine-readable per-device output:

```bash
hs --json fan PIXEL6,EMU -- wait "Welcome" --timeout 10s | jq -c
```

## 6. Screenshot on failure (use with `bash -e`)

```bash
set -e
trap 'hs see /tmp/handsets-failure-$$.jpg' ERR
hs tap "Sign in" --unique
hs wait "Dashboard" --timeout 10s
```

## 7. Selector recipes

Real Android UIs rarely give you stable resource IDs. Lean on the
relational pseudo-classes:

```bash
# The "OK" button that lives inside a particular dialog:
hs tap '*[text="OK"]:in(LinearLayout[id~=dialog])' --unique

# The EditText below the "Email" label:
hs tap '*EditText:below(TextView[text=Email]):visible' --unique

# Buttons near a specific icon:
hs find 'Button:near(ImageView[desc~=cart], 200)'

# OR groups: "Continue" or "Next" — whichever shipped this build:
hs tap '*[text=Continue], *[text=Next]' --visible --unique --timeout 5s
```

## 8. Composite "act and verify"

Tap a button and only succeed if the next screen really appears. No more
`tap ... && sleep 0.5 && wait ...` chains:

```bash
hs act --tap "Send" --until "Sent" --timeout 8s
hs act --type [id=q] "claude" --until '*RecyclerView' --timeout 5s
hs act --swipe up 300 --until-idle --timeout 2s
```

Returns exit 0 if both halves succeeded, TIMEOUT if the predicate never
fired, NOT_FOUND if the action couldn't even execute.

## 9. Long-running flow with shared defaults

`hs run` reads CLI verb lines from a file (or `-` for stdin) and executes
them over a single warm TCP socket. The `set` directives raise defaults
without touching every line:

```bash
hs run flow.hs
```

```text
# flow.hs
set timeout=8s
set retries=2
set continue-on-error          # don't bail on the first NOT_FOUND
set dump-ttl=200ms             # 200 ms cache window for selector lookups

wait idle
tap   "Continue"   --visible --unique
type  [id=email]   "you@example.com"
type  [id=pw]      "hunter2"
submit
wait  "Dashboard"  --timeout 15s
```

Use `hs init` to drop a starter `script.hs` you can hack on:

```bash
hs init my-flow.hs
hs run my-flow.hs
```

## 10. Scripting from Python / Node / Go

`hs` is language-agnostic. Drive it as a subprocess and parse `--json`:

```python
# Python — branch on exit code, parse JSON for the result payload.
import json, subprocess
r = subprocess.run(
    ["hs", "--json", "tap", "Sign in", "--visible", "--unique", "--timeout", "5s"],
    capture_output=True, text=True,
)
if r.returncode == 0:
    payload = json.loads(r.stdout)["result"]
    print("tapped at", payload["x"], payload["y"])
elif r.returncode == 2:
    raise RuntimeError("no Sign-in button on screen")
elif r.returncode == 3:
    raise TimeoutError("tap timed out")
else:
    raise RuntimeError(r.stderr)
```

For high-throughput drivers, keep one `hs run` subprocess open and feed it
verb lines on its stdin — that's the cheapest possible per-action overhead
because all the TCP / process / dump-parse costs are amortised.
