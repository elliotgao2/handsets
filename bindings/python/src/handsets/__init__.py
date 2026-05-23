"""Pythonic bindings for the Handsets Android control CLI.

The CLI (`hs`) does the hard work: warm daemon, push-mirrored state,
millisecond round-trips. This package wraps the CLI in a small,
context-managed Session class so Python callers stop reimplementing the
subprocess + JSON-parse + exit-code dance.

    from handsets import Session
    with Session() as d:
        d.tap("Continue")
        d.wait("Welcome", timeout="15s")

Errors come back as typed exceptions; see ``handsets.errors``.
"""

from .errors import (
    Ambiguous,
    BadArg,
    DaemonError,
    DeviceGone,
    ErrCode,
    HandsetsError,
    NotFound,
    Precondition,
    SecureWindow,
    Timeout,
    UnknownCmd,
)
from .session import Node, Session

__all__ = [
    "Session",
    "Node",
    # Exception hierarchy
    "HandsetsError",
    "NotFound",
    "Timeout",
    "Ambiguous",
    "DaemonError",
    "DeviceGone",
    "Precondition",
    "BadArg",
    "SecureWindow",
    "UnknownCmd",
    "ErrCode",
]

__version__ = "0.1.22"
