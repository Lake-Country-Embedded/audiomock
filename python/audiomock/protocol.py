"""Wire protocol for the audiomockd control socket.

Length-prefixed JSON over ``AF_UNIX``: a 4-byte big-endian payload length
followed by the JSON body. Requests and responses are serde-internally-tagged,
so every message carries a ``"type"`` discriminant.

Talking the protocol directly rather than shelling out to the ``audiomock`` CLI
is deliberate. The CLI exits 1 for every failure -- daemon down, no such device,
no PipeWire node matched the pattern are indistinguishable -- and it ignores
``--json`` on ``play``, ``generate``, ``record`` and ``stop``. Over the socket
those all come back as ``{"type": "Error", "message": ...}``, which is the
difference between a test that reports what went wrong and one that says
"exit status 1".
"""

from __future__ import annotations

import json
import os
import socket
import struct
from pathlib import Path
from typing import Any, Dict, Optional

__all__ = [
    "AudioMockError",
    "DaemonUnavailable",
    "default_socket_path",
    "request",
]

_HEADER = struct.Struct(">I")
_MAX_MESSAGE = 1 << 20  # matches the daemon's 1 MiB cap


class AudioMockError(RuntimeError):
    """The daemon rejected a request.

    Carries the daemon's own message, so failures read as "Device 'x' not found"
    rather than an exit code.
    """


class DaemonUnavailable(AudioMockError):
    """The control socket is missing or refused the connection.

    Distinct from :class:`AudioMockError` so fixtures can skip on "no daemon"
    while still failing on "daemon said no".
    """


def default_socket_path() -> Path:
    """``$XDG_RUNTIME_DIR/audiomockd.sock``, falling back to ``/tmp``."""
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR")
    return Path(runtime_dir if runtime_dir else "/tmp") / "audiomockd.sock"


def _recv_exactly(sock: socket.socket, count: int) -> bytes:
    chunks = []
    remaining = count
    while remaining:
        chunk = sock.recv(remaining)
        if not chunk:
            raise AudioMockError(
                f"daemon closed the connection after {count - remaining} of {count} bytes"
            )
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def request(
    payload: Dict[str, Any],
    *,
    socket_path: Optional[Path] = None,
    timeout: float = 30.0,
) -> Dict[str, Any]:
    """Send one request and return the decoded response.

    Raises :class:`DaemonUnavailable` when the socket cannot be reached and
    :class:`AudioMockError` when the daemon returns an ``Error`` response.
    """
    path = Path(socket_path) if socket_path else default_socket_path()

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(timeout)
    try:
        try:
            sock.connect(str(path))
        except (FileNotFoundError, ConnectionRefusedError, PermissionError) as exc:
            raise DaemonUnavailable(
                f"cannot reach audiomockd at {path}: {exc}. "
                "Start it with `audiomockd` or `systemctl --user start audiomockd`."
            ) from exc

        body = json.dumps(payload).encode()
        sock.sendall(_HEADER.pack(len(body)) + body)

        length = _HEADER.unpack(_recv_exactly(sock, _HEADER.size))[0]
        if length > _MAX_MESSAGE:
            raise AudioMockError(f"response of {length} bytes exceeds the {_MAX_MESSAGE} cap")
        response = json.loads(_recv_exactly(sock, length))
    except socket.timeout as exc:
        raise AudioMockError(f"audiomockd did not respond within {timeout}s") from exc
    finally:
        sock.close()

    if response.get("type") == "Error":
        raise AudioMockError(response.get("message", "unspecified daemon error"))
    return response
