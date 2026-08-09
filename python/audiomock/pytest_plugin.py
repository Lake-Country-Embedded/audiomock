"""Pytest fixtures for driving audiomock against an emulated DUT.

Registered as a ``pytest11`` entry point, so installing the package makes these
available everywhere.

``dut_audio`` is the one most tests want: a device pair created for the DUT,
exclusively linked to its PipeWire node, and torn down afterwards.

The DUT's node name comes from the ``--audiomock-node`` option, or is derived
from ``--host`` (``vk-mps-xp-aabb01.local`` -> ``vkdut-vk-mps-xp-aabb01``). Pass
it explicitly when the emulator was started under a different nickname.
"""

from __future__ import annotations

from pathlib import Path
from typing import Iterator, Optional

import pytest

from .client import AudioMock, AudioMockError, DaemonUnavailable
from .protocol import default_socket_path


def pytest_addoption(parser):
    group = parser.getgroup("audiomock")
    group.addoption(
        "--audiomock-socket",
        action="store",
        default=None,
        help="Path to the audiomockd control socket "
             "(default: $XDG_RUNTIME_DIR/audiomockd.sock)",
    )
    group.addoption(
        "--audiomock-node",
        action="store",
        default=None,
        help="PipeWire node name of the DUT, e.g. vkdut-mps01. Defaults to one "
             "derived from --host. Must be specific: the pattern is a substring "
             "match, so 'qemu' matches every running emulator.",
    )


@pytest.fixture(scope="session")
def audiomock_socket(request) -> Path:
    configured = request.config.getoption("--audiomock-socket")
    return Path(configured) if configured else default_socket_path()


@pytest.fixture(scope="session")
def audiomock_daemon(audiomock_socket) -> AudioMock:
    """A reachable audiomockd, or skip.

    Skips rather than fails: audiomock is QEMU-only by construction (it links
    host PipeWire nodes, and real hardware exposes none), so its absence is a
    property of the environment, not a defect.
    """
    client = AudioMock(audiomock_socket)
    try:
        status = client.status()
    except DaemonUnavailable as exc:
        pytest.skip(str(exc))
    except AudioMockError as exc:
        pytest.fail(f"audiomockd is reachable but unhealthy: {exc}")

    if not status.version:
        pytest.fail("audiomockd returned a status with no version")
    return client


@pytest.fixture(scope="session")
def audiomock_node_name(request) -> str:
    """PipeWire node name of the DUT under test."""
    explicit = request.config.getoption("--audiomock-node")
    if explicit:
        return explicit

    host = request.config.getoption("--host", default=None)
    if not host:
        pytest.skip("cannot infer the DUT's PipeWire node; pass --audiomock-node")

    # vkdev names the emulator's node vkdut-<nickname>, where the nickname is
    # the host without its mDNS suffix.
    nickname = str(host).split(".")[0]
    return nickname if nickname.startswith("vkdut-") else f"vkdut-{nickname}"


@pytest.fixture(scope="session")
def dut_audio(audiomock_daemon, audiomock_node_name) -> Iterator[AudioMock]:
    """A device pair exclusively linked to the DUT's emulated audio device.

    Creates a pair named after the DUT node so concurrent DUTs do not collide,
    links it exclusively -- otherwise WirePlumber routes the DUT to real
    speakers and a real microphone, silently mixing room noise into every
    capture -- and unlinks and destroys it afterwards.

    Fails rather than skips when the link cannot be made: a linked-looking but
    unlinked pair still produces valid all-zero recordings, and a test that
    "passes" on silence is worse than one that errors.
    """
    device = audiomock_node_name.replace("vkdut-", "") or "dut"
    client = AudioMock(audiomock_daemon.socket_path, device=device)

    existing = {d.name for d in audiomock_daemon.devices()}
    created_here = device not in existing
    if created_here:
        client.create_device(
            device,
            source_description=f"Virtual Mic ({device})",
            sink_description=f"Virtual Speaker ({device})",
        )

    try:
        client.link(node_pattern=audiomock_node_name, exclusive=True, require=True)
        yield client
    finally:
        try:
            client.stop()
        except AudioMockError:
            pass
        try:
            client.unlink()
        except AudioMockError:
            pass
        if created_here:
            try:
                client.destroy_device(device)
            except AudioMockError:
                pass


@pytest.fixture
def dut_audio_capture(dut_audio, tmp_path):
    """Factory for a capture of the DUT's audio output.

    ``factory(seconds=6, name="page.wav") -> contextmanager`` yielding a
    :class:`~audiomock.results.Capture`. Files land under ``tmp_path`` unless an
    absolute path is given.
    """
    def factory(seconds: float = 5.0, name: str = "capture.wav", **kwargs):
        target = Path(name)
        if not target.is_absolute():
            target = tmp_path / target
        return dut_audio.capture(target, seconds=seconds, **kwargs)

    return factory
