"""High-level client for audiomockd.

audiomock links a virtual source/sink pair into the PipeWire graph alongside a
running QEMU DUT, so the host can inject audio into the guest's microphone and
capture what the guest plays. ``vkdev dut start <nick>`` already names the
emulator's PipeWire node ``vkdut-<nick>`` for exactly this purpose.

Three things this wrapper does that raw calls do not, all of them lessons from
the tool's failure modes:

* **Link results are checked.** A failed link is silent: recording still yields
  a valid, well-formed, all-zero WAV. :meth:`link` raises unless links were
  actually created.
* **Links are exclusive by default.** WirePlumber will happily route the DUT to
  real speakers and a real microphone; without ``exclusive`` a capture quietly
  mixes room noise and the guest's audio also plays out of the physical
  speakers.
* **Recording is bounded and waited on.** The daemon emits no completion event
  (``Response::Completed`` is declared but never sent), so duration is enforced
  server-side and the client waits the same span before reading the file.

Analysis is deliberately absent. audiomock has no RMS, FFT, or beep detection --
those helpers exist only as private Rust test code -- and the test framework
already has proven implementations in ``common/audio/analysis.py``.
"""

from __future__ import annotations

import logging
import time
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Dict, Iterator, List, Optional, Union

from .protocol import AudioMockError, DaemonUnavailable, default_socket_path, request
from .results import Capture, DeviceInfo, LinkResult, Status

logger = logging.getLogger(__name__)

__all__ = ["AudioMock", "AudioMockError", "DaemonUnavailable"]

PathLike = Union[str, Path]

WAVEFORMS = ("Sine", "Square", "Sawtooth", "Noise")
OUTPUT_FORMATS = ("Wav", "Flac", "Ogg")


def _normalize(value: str, allowed: tuple, label: str) -> str:
    """Accept ``sine``/``SINE``/``Sine`` and return the wire spelling."""
    for candidate in allowed:
        if value.lower() == candidate.lower():
            return candidate
    raise ValueError(f"unknown {label} {value!r}; expected one of {allowed}")


class AudioMock:
    """Control one audiomockd instance."""

    def __init__(
        self,
        socket_path: Optional[PathLike] = None,
        *,
        device: str = "default",
        timeout: float = 30.0,
    ):
        self.socket_path = Path(socket_path) if socket_path else default_socket_path()
        self.device = device
        self.timeout = timeout

    # -- plumbing ----------------------------------------------------------

    def _call(self, payload: Dict[str, Any], *, timeout: Optional[float] = None) -> Dict[str, Any]:
        return request(
            payload,
            socket_path=self.socket_path,
            timeout=self.timeout if timeout is None else timeout,
        )

    def _device(self, device: Optional[str]) -> str:
        return device or self.device

    # -- status ------------------------------------------------------------

    def status(self) -> Status:
        return Status.from_json(self._call({"type": "Status"}))

    def devices(self) -> List[DeviceInfo]:
        return [DeviceInfo.from_json(d) for d in self._call({"type": "DevicesList"})["devices"]]

    def device_info(self, device: Optional[str] = None) -> Optional[DeviceInfo]:
        name = self._device(device)
        return next((d for d in self.devices() if d.name == name), None)

    def is_available(self) -> bool:
        """True if the daemon answers. Never raises."""
        try:
            self.status()
            return True
        except AudioMockError:
            return False

    # -- device lifecycle --------------------------------------------------

    def create_device(
        self,
        name: str,
        *,
        source_description: Optional[str] = None,
        sink_description: Optional[str] = None,
        autoconnect: bool = False,
    ) -> None:
        """Create a device pair. One per DUT when several run concurrently."""
        self._call({
            "type": "DevicesCreate",
            "name": name,
            "source_description": source_description,
            "sink_description": sink_description,
            "autoconnect": autoconnect,
        })

    def destroy_device(self, name: str) -> None:
        """Destroy a device pair.

        The daemon drops the pair without unlinking first, so call :meth:`unlink`
        before this to leave the graph clean.
        """
        self._call({"type": "DevicesDestroy", "name": name})

    # -- graph wiring ------------------------------------------------------

    def link(
        self,
        *,
        device: Optional[str] = None,
        node_pattern: str = "qemu",
        exclusive: bool = True,
        require: bool = True,
    ) -> LinkResult:
        """Wire the device pair to PipeWire nodes matching ``node_pattern``.

        Pass the DUT's node name (``vkdut-<nickname>``), not the default
        ``"qemu"``: the pattern is a case-insensitive substring over port names,
        so ``"qemu"`` matches *every* running emulator, and a nickname that is a
        prefix of another cross-links them.

        Raises when no links were created and ``require`` is set -- which is the
        default, because a silent no-link produces silent captures that pass.
        """
        name = self._device(device)
        result = LinkResult.from_json(self._call({
            "type": "Link",
            "device": name,
            "pattern": node_pattern,
            "exclusive": exclusive,
        }))
        logger.debug(
            "linked %s to %r: %d created, %d removed",
            name, node_pattern, result.created, result.removed,
        )
        if require and not result.linked:
            raise AudioMockError(
                f"no PipeWire links created for device {name!r} matching "
                f"{node_pattern!r}. Is the DUT running with audio enabled "
                f"(`vkdev dut start <nick>` without --no-audio)? Captures would "
                f"be silent and would pass anyway."
            )
        return result

    def unlink(self, *, device: Optional[str] = None) -> None:
        self._call({"type": "Unlink", "device": self._device(device)})

    # -- injection ---------------------------------------------------------

    def _guard_idle(self, device: Optional[str], what: str, on_busy: str) -> None:
        """Refuse, or clear, a *conflicting* job already running on the device.

        Starting a job while another is active silently replaces it, which is
        how a leaked ``play --loop-count 0`` can feed a DUT indefinitely without
        anything noticing: the leak is invisible to whoever starts the next job,
        and every later capture is contaminated. So make it impossible to start
        one without dealing with the existing job.

        Only jobs in the *same direction* conflict. Playback feeds the DUT's
        mic/line-in; ``Recording`` captures the DUT's speaker/line-out. They run
        on independent nodes, and capturing while injecting is the core pattern
        of every audio test here::

            with dut_audio_capture(seconds=6) as capture:
                inject("sustained")

        Treating a live ``Recording`` as a conflict would stop the capture the
        instant the tone started, and the test would then measure silence and
        report it as the DUT being silent. Anything that is not a ``Recording``
        is treated as conflicting, so an unrecognised future job kind still
        errs on the safe side.

        ``on_busy="stop"`` (default) stops the old job and logs it, keeping test
        runs deterministic while still surfacing the leak. ``on_busy="fail"``
        raises instead, for callers that would rather not paper over it.
        """
        if on_busy not in ("stop", "fail"):
            raise ValueError(f"on_busy must be 'stop' or 'fail', got {on_busy!r}")

        name = self._device(device)
        busy = next(
            (
                d
                for d in self.devices()
                if d.name == name
                and d.busy
                and not (d.active_job is not None and d.active_job.kind == "Recording")
            ),
            None,
        )
        if busy is None:
            return

        kind = busy.active_job.kind if busy.active_job else "unknown"
        message = (
            f"audiomock device {name!r} already had a {kind} job running when "
            f"{what} was requested; it was almost certainly leaked by an earlier "
            f"run (a looping play outlives the process that started it)"
        )
        if on_busy == "fail":
            raise AudioMockError(message)
        logger.warning("%s -- stopping it first", message)
        self.stop(device=name)

    def play(
        self,
        file_path: PathLike,
        *,
        device: Optional[str] = None,
        loop_count: Optional[int] = None,
        volume: float = 1.0,
        on_busy: str = "stop",
    ) -> None:
        """Play a file into the DUT's microphone/line-in.

        ``loop_count=0`` loops forever. Returns as soon as playback starts --
        the daemon sends no completion event, so a caller that needs the file to
        finish must wait for its known duration.

        Refuses to start on top of an existing job; see :meth:`_guard_idle`.
        """
        self._guard_idle(device, "play", on_busy)
        self._call({
            "type": "Play",
            "device": self._device(device),
            "file_path": str(Path(file_path).resolve()),
            "loop_count": loop_count,
            "volume": volume,
        })

    def generate(
        self,
        *,
        device: Optional[str] = None,
        waveform: str = "sine",
        frequency: float = 440.0,
        volume: float = 1.0,
        duration_secs: Optional[float] = None,
        on_busy: str = "stop",
    ) -> None:
        """Generate a tone into the DUT's microphone/line-in.

        With no ``duration_secs`` the tone runs until :meth:`stop`, so this is
        the other way to leak a permanent signal into a DUT. Guarded the same
        way as :meth:`play`.
        """
        self._guard_idle(device, "generate", on_busy)
        self._call({
            "type": "Generate",
            "device": self._device(device),
            "waveform": _normalize(waveform, WAVEFORMS, "waveform"),
            "frequency": frequency,
            "volume": volume,
            "duration_secs": duration_secs,
            # Accepted and ignored by the daemon; sent for wire completeness.
            "continuous": duration_secs is None,
        })

    def loopback(self, *, device: Optional[str] = None) -> None:
        """Route the pair's sink back into its source inside the daemon.

        Used as the positive control for capture health: with loopback on, a tone
        injected into the source must reappear on the sink. A direct
        guest-out to guest-in PipeWire link cannot do this -- PipeWire treats it
        as a scheduling cycle and feeds silence.
        """
        self._call({"type": "Loopback", "device": self._device(device)})

    def stop(self, *, device: Optional[str] = None) -> None:
        """Stop whatever the device is doing.

        Note this clears the source *and* sink job at once: a recording cannot be
        stopped without also ending a tone on the same pair.
        """
        self._call({"type": "Stop", "device": self._device(device)})

    # -- capture -----------------------------------------------------------

    def record(
        self,
        file_path: PathLike,
        *,
        device: Optional[str] = None,
        duration_secs: Optional[float] = None,
        output_format: Optional[str] = None,
        sample_rate: Optional[int] = None,
        channels: Optional[int] = None,
    ) -> Capture:
        """Start recording the DUT's emulated audio output.

        Returns immediately. With ``duration_secs`` the daemon stops and
        finalizes on its own; :meth:`capture` wraps the wait.
        """
        target = Path(file_path).resolve()
        target.parent.mkdir(parents=True, exist_ok=True)
        self._call({
            "type": "Record",
            "device": self._device(device),
            "file_path": str(target),
            "format": _normalize(output_format, OUTPUT_FORMATS, "output format")
            if output_format else None,
            "duration_secs": duration_secs,
            "sample_rate": sample_rate,
            "channels": channels,
        })
        return Capture(path=target, device=self._device(device),
                       requested_seconds=duration_secs)

    @contextmanager
    def capture(
        self,
        file_path: PathLike,
        *,
        device: Optional[str] = None,
        seconds: Optional[float] = None,
        settle: float = 0.3,
        **record_kwargs: Any,
    ) -> Iterator[Capture]:
        """Record for the duration of the ``with`` block.

        ::

            with audiomock.capture(tmp / "out.wav", seconds=6) as cap:
                voip_client.call("ext6010", uri, play=tone, hangup_after=5)
            assert dominant_frequency(cap.path) == approx(1000, abs=25)

        ``settle`` gives the recorder a moment to come up before the body runs,
        so the beginning of the audio under test is not clipped. There is no
        readiness event to wait on -- the daemon emits none -- so this is a
        documented, tunable delay rather than a hidden one.

        With ``seconds`` set the daemon owns the deadline and we wait it out;
        without it, recording stops when the block exits.
        """
        capture = self.record(file_path, device=device, duration_secs=seconds, **record_kwargs)
        if settle:
            time.sleep(settle)
        try:
            yield capture
        finally:
            if seconds is not None:
                # Server-side deadline; wait for it, then a moment to finalize.
                time.sleep(max(0.0, seconds - settle) + 0.5)
            try:
                self.stop(device=device)
            except AudioMockError:
                pass  # already stopped by the server-side deadline

    # -- health ------------------------------------------------------------

    def assert_capture_path_live(
        self,
        file_path: PathLike,
        *,
        device: Optional[str] = None,
        frequency: float = 1000.0,
        seconds: float = 1.5,
    ) -> Capture:
        """Positive control: prove the capture path actually carries audio.

        Enables in-daemon loopback, injects a tone, records it, and returns the
        capture for the caller to analyze. Pair with a silent baseline to satisfy
        the "no capture assertion without a negative control" rule -- without one,
        a broken link and a silent DUT are indistinguishable and the test passes
        either way.
        """
        self.loopback(device=device)
        self.generate(device=device, frequency=frequency)
        try:
            with self.capture(file_path, device=device, seconds=seconds) as capture:
                pass
        finally:
            self.stop(device=device)
        return capture
