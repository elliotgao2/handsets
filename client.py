#!/usr/bin/env python3
"""Tiny client for the hs daemon.

Usage:
  client.py ping
  client.py dump            # all windows
  client.py dump_active     # active window only
  client.py quit
"""

from __future__ import annotations

import socket
import struct
import sys


def call(cmd: str, host: str = "127.0.0.1", port: int = 9008, timeout: float = 10.0) -> bytes:
    payload = cmd.encode("ascii")
    with socket.create_connection((host, port), timeout=timeout) as s:
        s.sendall(struct.pack(">I", len(payload)) + payload)
        header = _recv_exact(s, 4)
        (n,) = struct.unpack(">I", header)
        return _recv_exact(s, n)


def _recv_exact(s: socket.socket, n: int) -> bytes:
    buf = bytearray()
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            raise IOError(f"short read: got {len(buf)}/{n}")
        buf.extend(chunk)
    return bytes(buf)


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    cmd = argv[1]
    body = call(cmd)
    sys.stdout.buffer.write(body)
    # Only add a trailing newline for text commands when stdout is a terminal,
    # otherwise binary payloads (screenshots) get corrupted by the extra byte.
    if sys.stdout.isatty() and not cmd.startswith("screenshot"):
        sys.stdout.buffer.write(b"\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
