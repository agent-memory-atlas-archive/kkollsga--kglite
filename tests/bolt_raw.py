"""A minimal raw Bolt client for protocol-level tests.

The official driver never sends a malformed or out-of-order message, so the
guard tests (authentication, message size, nesting) speak the wire format
directly. Only the pieces those tests need are implemented.
"""

from __future__ import annotations

import socket
import struct

SIG_HELLO = 0x01
SIG_GOODBYE = 0x02
SIG_RESET = 0x0F
SIG_RUN = 0x10
SIG_BEGIN = 0x11
SIG_COMMIT = 0x12
SIG_ROLLBACK = 0x13
SIG_DISCARD = 0x2F
SIG_PULL = 0x3F
SIG_LOGON = 0x6A
SIG_LOGOFF = 0x6B
SIG_SUCCESS = 0x70
SIG_RECORD = 0x71
SIG_IGNORED = 0x7E
SIG_FAILURE = 0x7F

# Bolt 5.4 (minor 4, major 5), then three empty proposals.
_VERSIONS = bytes([0, 0, 4, 5]) + bytes(12)


class Struct:
    def __init__(self, signature: int, *fields):
        self.signature = signature
        self.fields = fields


def _sized(tiny: int, base: int, n: int) -> bytes:
    if n < 16:
        return bytes([tiny | n])
    if n < 256:
        return bytes([base, n])
    if n < 65536:
        return bytes([base + 1]) + struct.pack(">H", n)
    return bytes([base + 2]) + struct.pack(">I", n)


def pack(value) -> bytes:
    """PackStream-encode ``value`` (None, bool, int, float, str, list, dict, Struct)."""
    if value is None:
        return b"\xc0"
    if value is True:
        return b"\xc3"
    if value is False:
        return b"\xc2"
    if isinstance(value, float):
        return b"\xc1" + struct.pack(">d", value)
    if isinstance(value, int):
        if -16 <= value <= 127:
            return struct.pack(">b", value)
        if -128 <= value < 128:
            return b"\xc8" + struct.pack(">b", value)
        if -32768 <= value < 32768:
            return b"\xc9" + struct.pack(">h", value)
        if -(2**31) <= value < 2**31:
            return b"\xca" + struct.pack(">i", value)
        return b"\xcb" + struct.pack(">q", value)
    if isinstance(value, str):
        raw = value.encode()
        return _sized(0x80, 0xD0, len(raw)) + raw
    if isinstance(value, list):
        return _sized(0x90, 0xD4, len(value)) + b"".join(pack(v) for v in value)
    if isinstance(value, dict):
        body = b"".join(pack(k) + pack(v) for k, v in value.items())
        return _sized(0xA0, 0xD8, len(value)) + body
    if isinstance(value, Struct):
        return bytes([0xB0 | len(value.fields), value.signature]) + b"".join(pack(f) for f in value.fields)
    raise TypeError(type(value))


def chunk(message: bytes) -> bytes:
    out = b""
    for start in range(0, len(message), 65535):
        piece = message[start : start + 65535]
        out += struct.pack(">H", len(piece)) + piece
    return out + b"\x00\x00"


def nested_lists(depth: int) -> bytes:
    """A PackStream value of ``depth`` nested one-element lists, built flat."""
    return b"\x91" * (depth - 1) + b"\x90"


class RawBolt:
    def __init__(self, host: str, port: int, timeout: float = 5.0):
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.sock.sendall(b"\x60\x60\xb0\x17" + _VERSIONS)
        self.version = self._recv_exact(4)

    def close(self) -> None:
        self.sock.close()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

    def _recv_exact(self, n: int) -> bytes:
        data = b""
        while len(data) < n:
            part = self.sock.recv(n - len(data))
            if not part:
                raise EOFError("connection closed")
            data += part
        return data

    def send(self, signature: int, *fields) -> None:
        self.send_raw(pack(Struct(signature, *fields)))

    def send_raw(self, message: bytes) -> None:
        self.sock.sendall(chunk(message))

    def recv(self) -> tuple[int, bytes]:
        """Next server message as (signature, body-after-signature); EOFError at close."""
        message = b""
        while True:
            (size,) = struct.unpack(">H", self._recv_exact(2))
            if size == 0:
                if message:
                    return message[1], message[2:]
                continue
            message += self._recv_exact(size)

    def request(self, signature: int, *fields) -> int:
        """Send and return the response signature (RECORDs skipped)."""
        self.send(signature, *fields)
        while True:
            sig, _ = self.recv()
            if sig != SIG_RECORD:
                return sig

    def hello(self) -> int:
        return self.request(SIG_HELLO, {"user_agent": "raw-test/1.0"})

    def logon(self, user: str, password: str) -> int:
        return self.request(SIG_LOGON, {"scheme": "basic", "principal": user, "credentials": password})

    def is_closed(self, wait: float = 3.0) -> bool:
        """True when the server ends the connection (EOF or reset) within ``wait``."""
        self.sock.settimeout(wait)
        try:
            while True:
                self.recv()
        except (EOFError, ConnectionError):
            return True
        except TimeoutError:
            return False
