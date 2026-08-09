"""Typed views over audiomockd responses."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional

__all__ = [
    "ActiveJob",
    "AudioFormat",
    "Capture",
    "DeviceInfo",
    "LinkResult",
    "Status",
]


@dataclass(frozen=True)
class AudioFormat:
    sample_rate: int
    channels: int
    sample_format: str

    @classmethod
    def from_json(cls, raw: Mapping[str, Any]) -> "AudioFormat":
        return cls(
            sample_rate=int(raw["sample_rate"]),
            channels=int(raw["channels"]),
            sample_format=str(raw["sample_format"]),
        )


@dataclass(frozen=True)
class ActiveJob:
    """What a device pair is currently doing, if anything.

    ``kind`` is one of ``Tone``, ``Playback``, ``Recording``, ``Streaming`` or
    ``Loopback``; ``detail`` carries that variant's fields.
    """

    kind: str
    detail: Dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_json(cls, raw: Any) -> Optional["ActiveJob"]:
        if raw is None:
            return None
        if isinstance(raw, str):
            return cls(kind=raw)  # unit variant, e.g. "Loopback"
        # Externally tagged: {"Tone": {...}}
        kind, detail = next(iter(raw.items()))
        return cls(kind=kind, detail=dict(detail) if isinstance(detail, dict) else {})


@dataclass(frozen=True)
class DeviceInfo:
    name: str
    source_description: str
    sink_description: str
    format: AudioFormat
    active_job: Optional[ActiveJob]

    @property
    def busy(self) -> bool:
        return self.active_job is not None

    @classmethod
    def from_json(cls, raw: Mapping[str, Any]) -> "DeviceInfo":
        return cls(
            name=str(raw["name"]),
            source_description=str(raw.get("source_description", "")),
            sink_description=str(raw.get("sink_description", "")),
            format=AudioFormat.from_json(raw["format"]),
            active_job=ActiveJob.from_json(raw.get("active_job")),
        )


@dataclass(frozen=True)
class Status:
    version: str
    uptime_secs: float
    devices: List[DeviceInfo]

    def device(self, name: str) -> Optional[DeviceInfo]:
        return next((d for d in self.devices if d.name == name), None)

    @classmethod
    def from_json(cls, raw: Mapping[str, Any]) -> "Status":
        return cls(
            version=str(raw.get("version", "")),
            uptime_secs=float(raw.get("uptime_secs", 0.0)),
            devices=[DeviceInfo.from_json(d) for d in raw.get("devices", [])],
        )


@dataclass(frozen=True)
class LinkResult:
    """Outcome of wiring a device pair into the PipeWire graph.

    ``created`` is the assertion that matters. A failed link is not an error --
    the daemon happily reports zero links made, recording still produces a
    valid all-zero WAV, and the test passes for the wrong reason. Always check
    :attr:`linked`.
    """

    device: str
    created: int
    removed: int

    @property
    def linked(self) -> bool:
        return self.created > 0

    @classmethod
    def from_json(cls, raw: Mapping[str, Any]) -> "LinkResult":
        return cls(
            device=str(raw.get("device", "")),
            created=int(raw.get("created", 0)),
            removed=int(raw.get("removed", 0)),
        )


@dataclass(frozen=True)
class Capture:
    """A finished recording of the DUT's emulated audio output.

    Analysis lives in the test framework (``common/audio/analysis.py``), not
    here: audiomock is a transport, and the DSP it would need already exists and
    is proven by the beep-feedback tests.
    """

    path: Path
    device: str
    requested_seconds: Optional[float]

    def exists(self) -> bool:
        return self.path.is_file() and self.path.stat().st_size > 0

    @property
    def size_bytes(self) -> int:
        return self.path.stat().st_size if self.path.is_file() else 0
