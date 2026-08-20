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

import logging
import subprocess
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
    """A reachable audiomockd.

    Fails rather than skips. A test that asks for this fixture has declared it
    needs injected or captured audio; if the daemon is gone that test is not
    "inapplicable here", it is unverified. Skipping made a dead daemon look like
    a clean run: audiomockd exited mid-session once and twenty line-in tests
    skipped while the suite still reported green, which is exactly the silent
    coverage loss these tests exist to prevent.

    Selection is the job of the ``requires_audiomock`` marker and the capability
    gates, not of this fixture. Deselect on a target that cannot run audiomock;
    do not let an absent daemon quietly excuse the tests.
    """
    client = AudioMock(audiomock_socket)
    try:
        status = client.status()
    except DaemonUnavailable as exc:
        pytest.fail(
            f"{exc}\n"
            f"These tests require audiomock; a missing daemon is a broken rig, "
            f"not a reason to pass. Start it with `audiomockd` (or "
            f"`systemctl --user start audiomockd` where the unit is installed), "
            f"or deselect the audiomock tests with -m 'not requires_audiomock'."
        )
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
        pytest.fail(
            "cannot infer the DUT's PipeWire node: pass --audiomock-node "
            "(or --host). Skipping here would hide the tests rather than run them."
        )

    # vkdev names the emulator's node vkdut-<nickname>, where the nickname is
    # the host without its mDNS suffix.
    nickname = str(host).split(".")[0]
    return nickname if nickname.startswith("vkdut-") else f"vkdut-{nickname}"


def _assert_no_physical_audio(node_name: str) -> None:
    """Fail if the DUT is wired to real hardware.

    Linking non-exclusively is only safe because `vkdev dut start` points the
    emulator at private null endpoints. Started by an older vkdev (or by hand
    without them), the DUT lands on the host's default sink and source instead:
    the developer's microphone feeds the device under test, so room noise sits
    inside every input-level and silence assertion, and the device plays out of
    their speakers. That is a wrong answer, not a flaky one, so refuse rather
    than measure it.
    """
    try:
        out = subprocess.run(["pw-link", "-l"], capture_output=True, text=True,
                             timeout=10)
    except (OSError, subprocess.SubprocessError):
        return  # no pw-link: nothing to check against, carry on
    if out.returncode != 0:
        return

    physical, seen_dut = [], False
    for line in out.stdout.splitlines():
        stripped = line.strip()
        if not line.startswith((" ", "\t")):
            seen_dut = stripped.startswith(f"{node_name}:")
            continue
        peer = stripped.lstrip("|<->").strip()
        if seen_dut and peer.startswith("alsa_"):
            physical.append(peer)

    if physical:
        pytest.fail(
            f"{node_name} is linked to physical audio hardware "
            f"({', '.join(sorted(set(physical)))}). The DUT must run on its own "
            f"null endpoints: restart it with `vkdev dut stop/start`, which "
            f"creates them. Measuring through the host's speakers and "
            f"microphone would mix room noise into every capture."
        )


@pytest.fixture(scope="session")
def dut_audio(audiomock_daemon, audiomock_node_name) -> Iterator[AudioMock]:
    """A device pair linked alongside the DUT's private null endpoints.

    Creates a pair named after the DUT node so concurrent DUTs do not collide,
    links it, and unlinks and destroys it afterwards.

    The link is deliberately **not** exclusive. It used to be, to stop
    WirePlumber routing the DUT to real speakers and a real microphone; that
    job now belongs to `vkdev dut start`, which gives each emulator its own
    null sink and virtual source and names them as QEMU's targets, so no
    physical device is ever in the path.

    Exclusivity actively broke things once that was in place. It tore down the
    null-endpoint links, and `unlink()` at session teardown left the emulator's
    output stream connected to nothing at all. QEMU drives AC97 DMA from its
    backend's process callback, so an unconsumed output stream stops the DMA:
    the guest's `hw_ptr` never leaves 0, writes fail with EIO, and every later
    capture is well-formed digital silence at -96 dBFS. The DUT stayed that way
    until it was restarted, so one session poisoned the next.

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
        # Drop our own stale links first. `link()` reports how many it
        # *created*, and connect() is a no-op on a link that already exists, so
        # a pair left behind by an earlier session reports created=0 and
        # require=True raises even though the DUT is correctly wired.
        # Exclusive mode never hit this because it cleared our side first; this
        # keeps that half without also tearing down the DUT's null endpoints.
        try:
            client.unlink()
        except AudioMockError:
            pass
        client.link(node_pattern=audiomock_node_name, exclusive=False, require=True)
        _assert_no_physical_audio(audiomock_node_name)

        # A job still running here was leaked by an earlier run or another
        # session, and would feed this DUT for the whole session: every
        # silence assertion would be meaningless and every capture polluted.
        # Clear it at the door rather than letting each test discover it.
        stale = next(
            (d for d in client.devices() if d.name == device and d.busy), None
        )
        if stale is not None:
            kind = stale.active_job.kind if stale.active_job else "unknown"
            logging.getLogger(__name__).warning(
                "audiomock device %r had a leaked %s job at session start; "
                "stopping it before any test runs", device, kind,
            )
            client.stop()

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
