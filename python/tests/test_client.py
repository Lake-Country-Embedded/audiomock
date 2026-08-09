"""Unit tests for the audiomock bindings. No daemon required."""

from __future__ import annotations

import json
import struct
from pathlib import Path

import pytest

from audiomock import AudioMock, AudioMockError, DaemonUnavailable
from audiomock.client import _normalize
from audiomock.protocol import default_socket_path
from audiomock.results import ActiveJob, DeviceInfo, LinkResult, Status

STATUS_PAYLOAD = {
    "type": "Status",
    "version": "0.1.0",
    "uptime_secs": 12.5,
    "devices": [
        {
            "name": "default",
            "source_description": "Virtual Mic",
            "sink_description": "Virtual Speaker",
            "format": {"sample_rate": 48000, "channels": 2, "sample_format": "F32LE"},
            "active_job": "Loopback",
        }
    ],
}


class _FakeTransport:
    """Captures requests and replays canned responses."""

    def __init__(self, responses):
        self.responses = list(responses)
        self.requests = []

    def __call__(self, payload, *, socket_path=None, timeout=None):
        self.requests.append(payload)
        response = self.responses.pop(0)
        if response.get("type") == "Error":
            raise AudioMockError(response.get("message", ""))
        return response


@pytest.fixture
def client(monkeypatch):
    def make(*responses):
        transport = _FakeTransport(responses)
        monkeypatch.setattr("audiomock.client.request", transport)
        instance = AudioMock(Path("/nonexistent.sock"), device="dut")
        instance.transport = transport  # test handle
        return instance
    return make


# --- results parsing --------------------------------------------------------

def test_status_parses_devices_and_unit_variant_job():
    status = Status.from_json(STATUS_PAYLOAD)
    assert status.version == "0.1.0"
    device = status.device("default")
    assert device is not None
    assert device.format.sample_rate == 48000
    assert device.active_job is not None
    assert device.active_job.kind == "Loopback"
    assert device.busy is True


def test_active_job_parses_struct_variant():
    job = ActiveJob.from_json({"Tone": {"waveform": "Sine", "frequency": 1000.0, "volume": 1.0}})
    assert job is not None
    assert job.kind == "Tone"
    assert job.detail["frequency"] == 1000.0


def test_active_job_none_means_idle():
    assert ActiveJob.from_json(None) is None
    assert DeviceInfo.from_json({
        "name": "d", "source_description": "", "sink_description": "",
        "format": {"sample_rate": 48000, "channels": 2, "sample_format": "F32LE"},
        "active_job": None,
    }).busy is False


def test_link_result_linked_requires_a_created_link():
    assert LinkResult.from_json({"device": "d", "created": 2, "removed": 0}).linked is True
    assert LinkResult.from_json({"device": "d", "created": 0, "removed": 3}).linked is False


# --- request construction ---------------------------------------------------

def test_link_defaults_to_exclusive(client):
    mock = client({"type": "Linked", "device": "dut", "created": 2, "removed": 1})
    mock.link(node_pattern="vkdut-mps01")

    sent = mock.transport.requests[0]
    assert sent["type"] == "Link"
    assert sent["pattern"] == "vkdut-mps01"
    assert sent["exclusive"] is True, (
        "links must be exclusive by default or WirePlumber routes the DUT to "
        "real hardware and captures pick up room noise"
    )


def test_link_raises_when_nothing_was_linked(client):
    """The failure mode that would otherwise produce silent, passing captures."""
    mock = client({"type": "Linked", "device": "dut", "created": 0, "removed": 0})
    with pytest.raises(AudioMockError, match="no PipeWire links created"):
        mock.link(node_pattern="vkdut-nope")


def test_link_can_be_permissive_when_asked(client):
    mock = client({"type": "Linked", "device": "dut", "created": 0, "removed": 0})
    result = mock.link(node_pattern="vkdut-nope", require=False)
    assert result.linked is False


def test_generate_normalizes_waveform_case(client):
    mock = client({"type": "GenerateStarted"})
    mock.generate(waveform="sine", frequency=440.0)
    assert mock.transport.requests[0]["waveform"] == "Sine"


def test_generate_rejects_an_unknown_waveform(client):
    mock = client({"type": "GenerateStarted"})
    with pytest.raises(ValueError, match="unknown waveform"):
        mock.generate(waveform="triangle")


def test_play_resolves_the_file_path(client, tmp_path):
    target = tmp_path / "tone.wav"
    target.write_bytes(b"")
    mock = client({"type": "PlayStarted"})
    mock.play(target)
    assert mock.transport.requests[0]["file_path"] == str(target.resolve())


def test_record_creates_the_parent_directory(client, tmp_path):
    target = tmp_path / "nested" / "deeper" / "out.wav"
    mock = client({"type": "RecordStarted"})
    capture = mock.record(target, duration_secs=2.0)
    assert target.parent.is_dir()
    assert capture.requested_seconds == 2.0
    assert mock.transport.requests[0]["duration_secs"] == 2.0


def test_device_override_beats_the_default(client):
    mock = client({"type": "Stopped"})
    mock.stop(device="other")
    assert mock.transport.requests[0]["device"] == "other"


def test_capture_stops_the_device_on_exit(client, tmp_path):
    mock = client({"type": "RecordStarted"}, {"type": "Stopped"})
    with mock.capture(tmp_path / "c.wav", settle=0) as capture:
        assert capture.device == "dut"
    assert [r["type"] for r in mock.transport.requests] == ["Record", "Stop"]


def test_capture_stops_even_when_the_body_raises(client, tmp_path):
    mock = client({"type": "RecordStarted"}, {"type": "Stopped"})
    with pytest.raises(RuntimeError):
        with mock.capture(tmp_path / "c.wav", settle=0):
            raise RuntimeError("boom")
    assert mock.transport.requests[-1]["type"] == "Stop"


# --- availability -----------------------------------------------------------

def test_is_available_is_false_without_a_daemon():
    assert AudioMock(Path("/nonexistent/audiomockd.sock")).is_available() is False


def test_missing_socket_raises_daemon_unavailable():
    with pytest.raises(DaemonUnavailable):
        AudioMock(Path("/nonexistent/audiomockd.sock")).status()


def test_default_socket_follows_xdg_runtime_dir(monkeypatch, tmp_path):
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path))
    assert default_socket_path() == tmp_path / "audiomockd.sock"
    monkeypatch.delenv("XDG_RUNTIME_DIR")
    assert default_socket_path() == Path("/tmp/audiomockd.sock")


def test_normalize_is_case_insensitive():
    assert _normalize("WAV", ("Wav", "Flac", "Ogg"), "format") == "Wav"
