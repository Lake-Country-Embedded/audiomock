"""Python bindings for audiomock, the virtual audio device daemon.

Injects audio into an emulated DUT's microphone/line-in and captures what it
plays, by linking a PipeWire source/sink pair to the QEMU node that
``vkdev dut start`` already names ``vkdut-<nickname>``.

Talks the daemon's ``AF_UNIX`` protocol directly rather than shelling out to the
CLI, so failures arrive as messages instead of exit code 1.

Audio *analysis* is not provided: audiomock is a transport. Use the consuming
project's DSP helpers on the captured WAV.

    from audiomock import AudioMock

    mock = AudioMock(device="mps01")
    mock.link(node_pattern="vkdut-mps01", exclusive=True)
    with mock.capture("out.wav", seconds=6) as capture:
        ...  # drive the DUT
    assert capture.exists()
"""

from .client import AudioMock
from .protocol import AudioMockError, DaemonUnavailable, default_socket_path
from .results import ActiveJob, AudioFormat, Capture, DeviceInfo, LinkResult, Status

__version__ = "0.1.0"

__all__ = [
    "ActiveJob",
    "AudioFormat",
    "AudioMock",
    "AudioMockError",
    "Capture",
    "DaemonUnavailable",
    "DeviceInfo",
    "LinkResult",
    "Status",
    "default_socket_path",
]
