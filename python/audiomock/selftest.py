"""Build validator for the audiomock daemon and these bindings.

Run before blaming a test fixture: it proves the daemon is up, the protocol
matches, jobs start and are reported, captures produce well-formed files, and
every failure mode raises rather than passing silently.

Needs no DUT, and is therefore explicit about its own limit: there is **no
host-only path from injection to capture**. The virtual source feeds the guest's
microphone, the virtual sink captures the guest's speaker, and loopback routes
sink back into source so a guest hears itself -- never source to sink. So a
daemon-only capture is correctly empty, and end-to-end audio verification needs a
linked DUT.

    audiomock-selftest [--socket PATH]

Exit code 0 when every check passes, 1 otherwise.
"""

from __future__ import annotations

import argparse
import sys
import tempfile
import time
import wave
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, List, Optional

from .client import AudioMock
from .protocol import AudioMockError, DaemonUnavailable, default_socket_path

SELFTEST_DEVICE = "audiomock-selftest"


@dataclass
class CheckResult:
    name: str
    polarity: str  # "pos" | "neg"
    ok: bool
    detail: str = ""


def _rms_dbfs(path: Path) -> float:
    """Rough RMS of a 16-bit mono/stereo WAV, in dBFS.

    Deliberately dependency-free: the selftest must run before anything else is
    installed. Real analysis belongs in the consuming project's DSP helpers.
    """
    import array
    import math

    with wave.open(str(path), "rb") as handle:
        frames = handle.readframes(handle.getnframes())
        width = handle.getsampwidth()

    if width != 2 or not frames:
        return -120.0

    samples = array.array("h")
    samples.frombytes(frames[: len(frames) // 2 * 2])
    if not samples:
        return -120.0

    mean_square = sum(s * s for s in samples) / len(samples)
    if mean_square <= 0:
        return -120.0
    return 20 * math.log10(math.sqrt(mean_square) / 32768.0)


def _with_device(client: AudioMock) -> None:
    existing = {d.name for d in client.devices()}
    if SELFTEST_DEVICE not in existing:
        client.create_device(SELFTEST_DEVICE)


def check_daemon_reachable(client: AudioMock) -> CheckResult:
    status = client.status()
    return CheckResult(
        "daemon answers Status", "pos", bool(status.version),
        f"version={status.version} uptime={status.uptime_secs:.0f}s "
        f"devices={len(status.devices)}",
    )


def check_bad_socket_is_reported(client: AudioMock) -> CheckResult:
    bogus = AudioMock(Path(tempfile.gettempdir()) / "audiomockd-does-not-exist.sock")
    try:
        bogus.status()
    except DaemonUnavailable as exc:
        return CheckResult("missing socket raises DaemonUnavailable", "neg", True, str(exc)[:70])
    return CheckResult("missing socket raises DaemonUnavailable", "neg", False,
                       "no exception raised")


def check_unknown_device_is_reported(client: AudioMock) -> CheckResult:
    try:
        client.stop(device="audiomock-no-such-device")
    except AudioMockError as exc:
        return CheckResult("unknown device is rejected", "neg", True, str(exc)[:70])
    return CheckResult("unknown device is rejected", "neg", False, "no exception raised")


def check_device_lifecycle(client: AudioMock) -> CheckResult:
    name = f"{SELFTEST_DEVICE}-lifecycle"
    client.create_device(name)
    present = any(d.name == name for d in client.devices())
    client.destroy_device(name)
    gone = all(d.name != name for d in client.devices())
    return CheckResult("device create/destroy round-trips", "pos", present and gone,
                       f"created={present} destroyed={gone}")


def check_capture_writes_a_wav(client: AudioMock) -> CheckResult:
    """Recording produces a well-formed WAV of the requested duration.

    Note what this deliberately does NOT check: that the audio has content.
    There is no host-only path from injection to capture -- the source feeds the
    *guest's* microphone and the sink captures the *guest's* speaker, and
    loopback routes sink back into source (guest hears itself), never source to
    sink. So daemon-only, an empty capture is the correct outcome.

    Verifying the audio path end to end requires a linked DUT, which is what
    the integration suite's negative control does.
    """
    _with_device(client)
    scoped = AudioMock(client.socket_path, device=SELFTEST_DEVICE)
    seconds = 1.0
    with tempfile.TemporaryDirectory() as tmp:
        target = Path(tmp) / "capture.wav"
        with scoped.capture(target, seconds=seconds):
            pass

        if not target.is_file():
            return CheckResult("capture writes a WAV", "pos", False, "no file written")

        try:
            with wave.open(str(target), "rb") as handle:
                rate = handle.getframerate()
                frames = handle.getnframes()
        except wave.Error as exc:
            return CheckResult("capture writes a WAV", "pos", False, f"unreadable: {exc}")

        duration = frames / rate if rate else 0.0
        # Generous bound: the point is "roughly the requested span", not timing.
        ok = 0.5 * seconds <= duration <= 2.0 * seconds
        return CheckResult("capture writes a WAV", "pos", ok,
                           f"{duration:.2f}s at {rate} Hz ({target.stat().st_size} bytes)")


def check_unlinked_capture_is_silent(client: AudioMock) -> CheckResult:
    """An unlinked pair captures silence -- and still yields a valid WAV.

    This documents the tool's most dangerous property as an assertion: a capture
    that never had a link produces a well-formed, all-zero file. A test asserting
    only "we heard X" therefore cannot distinguish a broken rig from a quiet DUT,
    which is why every capture assertion must carry a negative control.
    """
    _with_device(client)
    scoped = AudioMock(client.socket_path, device=SELFTEST_DEVICE)
    with tempfile.TemporaryDirectory() as tmp:
        target = Path(tmp) / "silence.wav"
        scoped.stop()
        time.sleep(0.2)
        with scoped.capture(target, seconds=1.0):
            pass

        if not target.is_file():
            return CheckResult("unlinked capture is silent", "neg", False, "no file written")
        level = _rms_dbfs(target)
        return CheckResult("unlinked capture is silent", "neg", level < -60.0,
                           f"rms={level:.1f} dBFS, file is valid -- hence the "
                           f"negative-control rule")


def check_jobs_are_reported(client: AudioMock) -> CheckResult:
    """Starting a job shows up in the device's ``active_job``.

    The control-plane equivalent of "did the injection start", which is what a
    test can check without a DUT attached.
    """
    _with_device(client)
    scoped = AudioMock(client.socket_path, device=SELFTEST_DEVICE)
    try:
        scoped.stop()
        idle = scoped.device_info()
        scoped.generate(frequency=1000.0)
        time.sleep(0.2)
        busy = scoped.device_info()
    finally:
        scoped.stop()

    idle_job = idle.active_job if idle else None
    busy_job = busy.active_job if busy else None
    ok = idle_job is None and busy_job is not None and busy_job.kind == "Tone"
    return CheckResult("active_job reflects a started job", "pos", ok,
                       f"idle={idle_job} busy={busy_job.kind if busy_job else None}")


def check_link_failure_is_loud(client: AudioMock) -> CheckResult:
    """A pattern matching no node must raise, not silently produce nothing."""
    _with_device(client)
    scoped = AudioMock(client.socket_path, device=SELFTEST_DEVICE)
    try:
        scoped.link(node_pattern="audiomock-no-such-pipewire-node", require=True)
    except AudioMockError as exc:
        return CheckResult("unmatched link pattern raises", "neg", True, str(exc)[:70])
    finally:
        try:
            scoped.unlink()
        except AudioMockError:
            pass
    return CheckResult("unmatched link pattern raises", "neg", False,
                       "link reported success with no matching node")


CHECKS: List[Callable[[AudioMock], CheckResult]] = [
    check_daemon_reachable,
    check_bad_socket_is_reported,
    check_unknown_device_is_reported,
    check_device_lifecycle,
    check_jobs_are_reported,
    check_capture_writes_a_wav,
    check_unlinked_capture_is_silent,
    check_link_failure_is_loud,
]


def run_all(socket_path: Optional[Path] = None) -> List[CheckResult]:
    client = AudioMock(socket_path or default_socket_path())
    results: List[CheckResult] = []
    for check in CHECKS:
        try:
            results.append(check(client))
        except Exception as exc:  # a crashing check is a failing check
            results.append(CheckResult(check.__name__, "pos", False, f"{type(exc).__name__}: {exc}"))
    try:
        client.destroy_device(SELFTEST_DEVICE)
    except AudioMockError:
        pass
    return results


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="Validate audiomockd and these bindings")
    parser.add_argument("--socket", type=Path, default=None,
                        help="control socket path (default: $XDG_RUNTIME_DIR/audiomockd.sock)")
    args = parser.parse_args(argv)

    results = run_all(args.socket)
    for result in results:
        status = "PASS" if result.ok else "FAIL"
        print(f"  [{status}] ({result.polarity}) {result.name:<42} {result.detail}")

    passed = sum(1 for r in results if r.ok)
    failed = len(results) - passed
    print(f"\n{passed} passed, {failed} failed")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
