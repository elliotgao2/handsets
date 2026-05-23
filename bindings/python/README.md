# handsets — Python bindings

A small, Pythonic wrapper around the [Handsets](https://github.com/elliotgao2/handsets)
CLI (`hs`). Drives Android devices from Python without reimplementing the
subprocess + JSON-parse + exit-code boilerplate every caller used to write
by hand.

## Install

```bash
pip install handsets
```

You also need the `hs` binary on `$PATH`. See the project
[install instructions](https://github.com/elliotgao2/handsets#install).

## Usage

```python
from handsets import Session

with Session() as d:                   # `hs use` on enter, `hs drop` on exit
    for node in d.ui():
        print(node.cls, node.text, node.coords)

    d.tap("Continue")                  # text lookup
    d.tap(540, 860)                    # raw coords
    d.fill("EditText", "you@x.com")    # atomic ACTION_SET_TEXT against the selector
    d.type("hello")                    # keystrokes to the focused field
    d.submit()
    d.wait("Welcome", timeout="15s")
```

Errors map to typed exceptions:

```python
from handsets import Session, NotFound, Timeout, Ambiguous

try:
    d.tap("Submit", unique=True, timeout="5s")
except NotFound:
    ...  # exit code 2 — selector matched nothing
except Timeout:
    ...  # exit code 3 — wait budget exhausted
except Ambiguous:
    ...  # exit code 4 — --unique saw multiple matches
```

Everything else (daemon errors, bad arguments, secure-window blocks)
raises a generic `HandsetsError` whose `.code` attribute carries the
structured `ErrCode` enum value from the CLI's JSON output.

## Talking to a specific device

```python
Session(serial="PIXEL6_SERIAL")
```

Multiple sessions can run side-by-side; each one shells out independently.

## Why a thin wrapper?

The CLI already does the hard work: warm daemon, push-mirrored state,
millisecond round-trips. The Python layer's job is to make that ergonomic
— context managers, typed exceptions, no manual `subprocess.run`. Future
versions may keep an `hs run` subprocess warm and stream commands over its
stdin to amortise per-call process overhead.
