# audiomock Python bindings

Drive [audiomock](../README.md) from Python, and from pytest.

audiomock links a virtual PipeWire source/sink pair to a running QEMU DUT, so the
host can inject audio into the guest's microphone / line-in and capture whatever
the guest plays. `vkdev dut start <nick>` already names the emulator's PipeWire
node `vkdut-<nick>` for exactly this purpose.

## Install

```bash
pip install -e tools/audiomock/python
```

Installing registers a `pytest11` plugin, so the fixtures below are available
with no conftest wiring.

## Why the socket, not the CLI

These bindings speak the daemon's `AF_UNIX` protocol (4-byte big-endian length +
JSON) directly. The CLI exits `1` for every failure — daemon down, no such
device, and no matching PipeWire node are indistinguishable — and it accepts but
ignores `--json` on `play`, `generate`, `record` and `stop`. Over the socket
those all arrive as `{"type": "Error", "message": ...}`.

## Quick start

```python
from audiomock import AudioMock

mock = AudioMock(device="mps01")
mock.link(node_pattern="vkdut-mps01", exclusive=True)   # raises if nothing linked

with mock.capture("page.wav", seconds=6) as capture:
    ...                                                  # drive the DUT here

assert capture.exists()
```

Injecting into the guest's microphone:

```python
mock.generate(frequency=440.0, volume=0.8)   # runs until stop()
mock.play("prompt.wav", loop_count=1)
mock.stop()
```

## pytest fixtures

| Fixture | Scope | What it gives you |
|---|---|---|
| `audiomock_daemon` | session | A reachable daemon, or a clean skip |
| `audiomock_node_name` | session | The DUT's PipeWire node, from `--audiomock-node` or `--host` |
| `dut_audio` | session | A device pair exclusively linked to the DUT, torn down afterwards |
| `dut_audio_capture` | function | `factory(seconds=6, name="out.wav")` → capture context manager |

Options: `--audiomock-socket`, `--audiomock-node`.

```python
def test_page_reaches_line_out(dut_audio_capture, voip_client, sip_uri):
    with dut_audio_capture(seconds=6, name="page.wav") as capture:
        voip_client.call("ext6010", sip_uri("zone1"), play=TONE, hangup_after=5)

    assert dominant_frequency(capture.path) == pytest.approx(1000, abs=25)
```

## Three things that will bite you

**A failed link is silent.** Recording an unlinked pair still produces a valid,
well-formed, all-zero WAV. A test asserting only "we heard X" cannot tell a
broken rig from a quiet DUT. `link()` therefore raises unless links were actually
created, and **every capture assertion should carry a negative control** — a
silent baseline, or a known-tone positive control in the same session.

**WirePlumber will fight you.** Without `exclusive=True` the session manager
keeps the DUT wired to real speakers and a real microphone, so captures quietly
mix room noise and the guest's audio also plays out of your desk speakers.
`link()` is exclusive by default.

**There is no completion event.** `Response::Completed` and `Progress` are
declared in the protocol and never sent. Playback and recording return as soon as
they start, so durations are enforced server-side and waited out client-side.
`capture(seconds=...)` handles this; anything else needs an explicit,
documented wait.

## Analysis lives elsewhere

audiomock is a transport. It has no RMS, FFT, beep detection or timing
measurement — those helpers exist only as private Rust test code in
`crates/audiomock-integration`. Analyze the captured WAV with the consuming
project's DSP helpers (in this monorepo, `testing/common/audio/analysis.py`,
which already provides dominant-frequency, RMS, presence and beep detection).

## Selftest

```bash
audiomock-selftest
```

Eight checks: daemon reachable, missing socket reported, unknown device rejected,
device lifecycle, jobs reflected in `active_job`, captures well-formed, unlinked
captures silent, unmatched link patterns raise.

It needs no DUT, and is explicit about the limit that implies: **there is no
host-only path from injection to capture.** The source feeds the *guest's* mic,
the sink captures the *guest's* speaker, and loopback routes sink back into
source so a guest hears itself — never source to sink. A daemon-only capture is
correctly empty; end-to-end audio verification needs a linked DUT.

## Tests

```bash
pytest tools/audiomock/python/tests -q      # unit tests, no daemon needed
```
