# audiomock

Virtual audio device emulator for testing products in QEMU that use ALSA audio. Creates PipeWire virtual source/sink streams that QEMU connects to, enabling automated audio testing without physical hardware.

## What it does

- **`audiomockd`** — daemon that creates PipeWire virtual audio devices (source + sink pairs)
- **`audiomock`** — CLI tool to generate tones, play files, record audio, stream raw PCM, and link to QEMU

Audio flows through PipeWire:

```
Host: audiomock generate 440Hz → PipeWire source → QEMU mic input
VM:   alsasrc → [your application] → alsasink
Host: QEMU speaker output → PipeWire sink → audiomock record
```

## Prerequisites

```bash
./scripts/setup.sh
```

Or manually:

```bash
sudo apt install -y \
  libpipewire-0.3-dev libspa-0.2-dev \
  pipewire pipewire-audio-client-libraries wireplumber \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  libclang-dev pkg-config

systemctl --user enable --now pipewire pipewire-pulse wireplumber
```

Requires Rust 1.70+ (install via [rustup](https://rustup.rs)).

## Install

```bash
./scripts/install.sh
systemctl --user enable --now audiomockd
```

Installs binaries to `~/.local/bin`, config to `~/.config/audiomock/config.toml`, and a systemd user service.

## Uninstall

```bash
./scripts/uninstall.sh
```

## Quick start

```bash
# Check daemon status
audiomock status

# Generate a 440Hz sine tone on the virtual source
audiomock generate --frequency 440 --continuous

# Record 5 seconds from the virtual sink
audiomock record /tmp/capture.wav --duration 5

# Stop active jobs
audiomock stop --device default
```

## QEMU usage

Start QEMU with PipeWire audio:

```bash
qemu-system-aarch64 \
  -audiodev pipewire,id=snd0 \
  -device ac97,audiodev=snd0 \
  ...
```

Link QEMU to the virtual devices:

```bash
audiomock link --device default
```

Then generate/record/play as needed. Inside the VM, audio appears on standard ALSA devices.

## CLI reference

```
audiomock status                          Show daemon info and active jobs
audiomock devices list|create|destroy     Manage virtual device pairs
audiomock generate [--frequency Hz]       Generate test tones (sine/square/sawtooth/noise)
audiomock play <file>                     Play WAV/MP3/FLAC/OGG to virtual source
audiomock record <file> [--duration s]    Record from virtual sink
audiomock stream --direction in|out       Raw PCM streaming via stdin/stdout
audiomock link                            Link QEMU PipeWire nodes to virtual devices
audiomock stop --device <name>            Stop active job on a device
```

All commands accept `--device <name>` (default: `default`) and `--json` for machine-readable output.

## Configuration

`~/.config/audiomock/config.toml`:

```toml
[daemon]
log_level = "info"

[audio]
default_sample_rate = 48000
default_channels = 2
default_sample_format = "F32LE"
buffer_size = 1024

[[device_pairs]]
name = "qemu-audio-0"
source_description = "QEMU Virtual Mic 0"
sink_description = "QEMU Virtual Speaker 0"
```

Device pairs can also be created at runtime:

```bash
audiomock devices create my-device --source-description "My Mic" --sink-description "My Speaker"
```

## Testing

```bash
cargo test --workspace -- --test-threads=1
```

24 tests: protocol serialization, ring buffer, and integration tests that start the daemon, exercise all commands, and validate audio with GStreamer frequency analysis.

## Architecture

```
crates/
  audiomock-proto/     Shared IPC protocol, audio types, config (TOML)
  audiomockd/          Daemon: PipeWire streams, IPC server (tokio), audio processing (GStreamer)
  audiomock/           CLI: subcommands, IPC client
  audiomock-integration/  Integration tests
```

The daemon runs PipeWire on a dedicated thread (pipewire-rs types are `!Send`) and communicates with the tokio async IPC server via crossbeam channels. Audio processing (tone generation, file decode/encode) uses GStreamer.

## License

See [LICENSE](LICENSE).
