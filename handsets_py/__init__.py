"""Persistent-socket client library for the handsets daemon.

Usage:
    from hs import HsClient
    c = HsClient()
    tree = c.dump_json()              # dict, all windows
    img  = c.screenshot("webp")        # bytes
"""

from __future__ import annotations

import json
import socket
import struct
from typing import Any


class HsClient:
    def __init__(self, host: str = "127.0.0.1", port: int = 9008, timeout: float = 10.0):
        self._sock = socket.create_connection((host, port), timeout=timeout)
        self._sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)

    def close(self) -> None:
        try:
            self._sock.close()
        except OSError:
            pass

    def __enter__(self) -> "HsClient":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()

    def _call(self, cmd: str) -> bytes:
        payload = cmd.encode("ascii")
        self._sock.sendall(struct.pack(">I", len(payload)) + payload)
        header = self._recv_exact(4)
        (n,) = struct.unpack(">I", header)
        return self._recv_exact(n)

    def _recv_exact(self, n: int) -> bytes:
        buf = bytearray()
        while len(buf) < n:
            chunk = self._sock.recv(min(65536, n - len(buf)))
            if not chunk:
                raise IOError(f"short read: got {len(buf)}/{n}")
            buf.extend(chunk)
        return bytes(buf)

    # --- a11y tree ---

    def ping(self) -> bytes:
        return self._call("ping")

    def dump_json(self) -> dict:
        return json.loads(self._call("dump"))

    def dump_active_json(self) -> dict:
        return json.loads(self._call("dump_active"))

    # --- screenshots ---

    def screenshot(self, fmt: str = "webp") -> bytes:
        """fmt in {'webp', 'png', 'raw'}."""
        if fmt == "raw":
            return self._call("screenshot_raw")
        if fmt == "png":
            return self._call("screenshot_png")
        return self._call("screenshot")

    def screenshot_raw(self) -> tuple[int, int, bytes]:
        """Returns (width, height, rgba_bytes). Strips the 16-byte header."""
        body = self._call("screenshot_raw")
        if body[:4] != b"A11Y":
            raise IOError(f"bad raw header: {body[:16]!r}")
        w, h, fmt = struct.unpack(">III", body[4:16])
        if fmt != 1:
            raise IOError(f"unexpected format {fmt}")
        return w, h, body[16:]
