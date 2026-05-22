"""Session — context-managed driver around the ``hs`` CLI.

Each method shells out one ``hs --json`` invocation, parses the single
JSON line that comes back, and either returns the ``result`` payload or
raises a typed exception. The implementation is intentionally simple: the
expensive bits (warm daemon, state mirror) live in the CLI, not here.

Future work: keep an ``hs run -`` subprocess warm across calls and write
verb lines to its stdin to amortise the ~5 ms per-process startup. The
API surface won't change.
"""

from __future__ import annotations

import json
import shutil
import subprocess
from dataclasses import dataclass
from typing import Iterable, List, Optional, Union

from .errors import HandsetsError, from_payload

Duration = Union[int, float, str]
"""A wait budget. Integers/floats are milliseconds; strings accept the
same ``250ms`` / ``5s`` suffixes the CLI's ``--timeout`` flag does."""


@dataclass(frozen=True)
class Node:
    """One row from ``hs ui`` / ``hs find``."""

    cls: str
    id: str
    text: str
    desc: str
    flags: str
    x1: int = 0
    y1: int = 0
    x2: int = 0
    y2: int = 0

    @property
    def coords(self) -> tuple[int, int]:
        """Centre point in pixels."""
        return ((self.x1 + self.x2) // 2, (self.y1 + self.y2) // 2)

    @property
    def clickable(self) -> bool:
        return "c" in self.flags

    @property
    def visible(self) -> bool:
        return "v" in self.flags


def _fmt_duration(d: Duration) -> str:
    """Normalise to the ``hs`` flag form (``250ms`` / ``5s`` / bare-ms-int)."""
    if isinstance(d, str):
        return d
    return f"{int(d)}ms"


class Session:
    """A connected device. Use as a context manager.

    >>> with Session() as d:
    ...     d.tap("Continue")
    ...     d.wait("Welcome", timeout="15s")
    """

    def __init__(
        self,
        serial: Optional[str] = None,
        *,
        binary: str = "hs",
        auto_connect: bool = True,
    ) -> None:
        self.serial = serial
        self.binary = binary
        self._connected = False
        self._auto_connect = auto_connect
        if shutil.which(binary) is None:
            raise FileNotFoundError(
                f"`{binary}` not on $PATH — install handsets first "
                "(see https://github.com/elliotgao2/handsets#install)"
            )

    # ─── context manager ──────────────────────────────────────────────

    def __enter__(self) -> "Session":
        if self._auto_connect:
            self.connect()
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        if self._connected:
            try:
                self.disconnect()
            except HandsetsError:
                # Best-effort teardown — don't mask the real exception.
                pass

    # ─── lifecycle ────────────────────────────────────────────────────

    def connect(self) -> None:
        """`hs use [serial]` — start the daemon and host-side state mirror."""
        argv = ["use"]
        if self.serial is not None:
            argv += ["--device", self.serial]
        self._call_text(argv)
        self._connected = True

    def disconnect(self, keep_jar: bool = False) -> None:
        """`hs drop` — tear the daemon down."""
        argv = ["drop"]
        if self.serial is not None:
            argv += ["--device", self.serial]
        if keep_jar:
            argv.append("--keep-jar")
        self._call_text(argv)
        self._connected = False

    # ─── inspection ───────────────────────────────────────────────────

    def ui(self) -> List[Node]:
        """Return one :class:`Node` per actionable element on the active window."""
        # `hs find '*'` returns every node; --json gives one structured line each.
        return self.find("*")

    def find(
        self,
        selector: str,
        *,
        visible: bool = False,
        clickable: bool = False,
        enabled: bool = False,
        unique: bool = False,
        nth: Optional[int] = None,
        timeout: Optional[Duration] = None,
    ) -> List[Node]:
        argv = ["find", selector]
        argv += self._action_flags(
            visible=visible, clickable=clickable, enabled=enabled,
            unique=unique, nth=nth, timeout=timeout,
        )
        rows = self._call_json_lines(argv)
        return [self._node_from_payload(r["result"]) for r in rows if r.get("ok")]

    def info(self) -> dict:
        """Return the cached device snapshot as a dict."""
        argv = ["show"]
        if self.serial is not None:
            argv = ["--device", self.serial] + argv
        proc = subprocess.run(
            [self.binary, *argv], capture_output=True, text=True, check=False,
        )
        # `hs show` (bare) is a text neofetch-style dump — surface as-is.
        if proc.returncode != 0:
            raise HandsetsError(proc.stderr.strip(), verb="show")
        return {"raw": proc.stdout}

    # ─── input ────────────────────────────────────────────────────────

    def tap(
        self,
        target: Union[str, int],
        y: Optional[int] = None,
        *,
        visible: bool = False,
        clickable: bool = False,
        enabled: bool = False,
        unique: bool = False,
        nth: Optional[int] = None,
        timeout: Optional[Duration] = None,
        retries: int = 0,
        retry_delay: Optional[Duration] = None,
    ) -> dict:
        """Tap by text/selector (one arg) or coordinates (``tap(x, y)``)."""
        if y is not None:
            argv = ["tap", str(target), str(y)]
        else:
            argv = ["tap", str(target)]
        argv += self._action_flags(
            visible=visible, clickable=clickable, enabled=enabled,
            unique=unique, nth=nth, timeout=timeout,
            retries=retries, retry_delay=retry_delay,
        )
        return self._call_one_json(argv, verb="tap")

    def type(
        self,
        selector_or_text: str,
        text: Optional[str] = None,
        *,
        timeout: Optional[Duration] = None,
    ) -> dict:
        """``type(TEXT)`` types into the focused field; ``type(SELECTOR, TEXT)``
        targets a specific node via ``ACTION_SET_TEXT`` (atomic, bypasses IME)."""
        if text is None:
            argv = ["type", selector_or_text]
        else:
            argv = ["type", selector_or_text, text]
        argv += self._action_flags(timeout=timeout)
        return self._call_one_json(argv, verb="type")

    def submit(self, selector: Optional[str] = None) -> dict:
        """Press the focused field's IME action key (Go / Search / Send / Done)."""
        argv = ["submit"]
        if selector is not None:
            argv.append(selector)
        return self._call_one_json(argv, verb="submit")

    def paste(self, selector: Optional[str] = None) -> dict:
        argv = ["paste"]
        if selector is not None:
            argv.append(selector)
        return self._call_one_json(argv, verb="paste")

    def go(self, key: str) -> dict:
        """Key event by name (``back``, ``home``, ``recents``, ``enter``, …)."""
        return self._call_one_json(["go", key], verb="go")

    def swipe(self, direction_or_x1, *args, duration_ms: Optional[int] = None) -> dict:
        argv = ["swipe", str(direction_or_x1), *[str(a) for a in args]]
        if duration_ms is not None:
            argv.append(str(duration_ms))
        return self._call_one_json(argv, verb="swipe")

    # ─── synchronisation ──────────────────────────────────────────────

    def wait(
        self,
        spec: str,
        *,
        timeout: Optional[Duration] = None,
        retries: int = 0,
    ) -> dict:
        argv = ["wait", spec]
        argv += self._action_flags(timeout=timeout, retries=retries)
        return self._call_one_json(argv, verb="wait")

    # ─── internals ────────────────────────────────────────────────────

    def _action_flags(
        self,
        *,
        visible: bool = False,
        clickable: bool = False,
        enabled: bool = False,
        unique: bool = False,
        nth: Optional[int] = None,
        timeout: Optional[Duration] = None,
        retries: int = 0,
        retry_delay: Optional[Duration] = None,
    ) -> List[str]:
        out: List[str] = []
        if visible:   out.append("--visible")
        if clickable: out.append("--clickable")
        if enabled:   out.append("--enabled")
        if unique:    out.append("--unique")
        if nth is not None: out += ["--nth", str(nth)]
        if timeout is not None: out += ["--timeout", _fmt_duration(timeout)]
        if retries:   out += ["--retries", str(retries)]
        if retry_delay is not None:
            out += ["--retry-delay", _fmt_duration(retry_delay)]
        return out

    def _argv_prefix(self) -> List[str]:
        argv: List[str] = [self.binary, "--json"]
        if self.serial is not None:
            argv += ["--device", self.serial]
        return argv

    def _call_text(self, argv: Iterable[str]) -> str:
        proc = subprocess.run(
            [self.binary, *argv], capture_output=True, text=True, check=False,
        )
        if proc.returncode != 0:
            raise HandsetsError(
                proc.stderr.strip() or proc.stdout.strip(),
                verb=str(next(iter(argv), "?")),
            )
        return proc.stdout

    def _call_one_json(self, argv: Iterable[str], *, verb: str) -> dict:
        rows = self._call_json_lines(argv)
        if not rows:
            raise HandsetsError("no JSON line on stdout", verb=verb)
        row = rows[-1]
        if not row.get("ok"):
            raise from_payload(verb, row.get("error", {}))
        return row.get("result", {})

    def _call_json_lines(self, argv: Iterable[str]) -> List[dict]:
        proc = subprocess.run(
            [*self._argv_prefix(), *argv],
            capture_output=True, text=True, check=False,
        )
        rows: List[dict] = []
        for line in proc.stdout.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                # Non-JSON lines slip through for verbs that don't honour
                # --json yet — ignore them; the exit code will decide.
                continue
        if proc.returncode != 0 and not rows:
            raise HandsetsError(
                proc.stderr.strip() or f"exit {proc.returncode}",
                verb=str(next(iter(argv), "?")),
            )
        return rows

    @staticmethod
    def _node_from_payload(p: dict) -> Node:
        return Node(
            cls=p.get("class", ""),
            id=p.get("id", ""),
            text=p.get("text", ""),
            desc=p.get("desc", ""),
            flags=p.get("flags", ""),
            x1=int(p.get("x1", 0) or 0),
            y1=int(p.get("y1", 0) or 0),
            x2=int(p.get("x2", 0) or 0),
            y2=int(p.get("y2", 0) or 0),
        )
