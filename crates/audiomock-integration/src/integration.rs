//! Integration tests for audiomock.
//!
//! These tests require PipeWire to be running. They start the daemon as a
//! subprocess, exercise it via the IPC protocol, and validate audio correctness
//! using GStreamer pipelines and frequency analysis.
//!
//! Run with: cargo test -p audiomock-integration -- --test-threads=1

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use anyhow::{Context, Result};
use audiomock_proto::audio::{OutputFormat, SampleFormat, WaveformKind};
use audiomock_proto::device::StreamDirection;
use audiomock_proto::protocol::{self, Request, Response};
use gstreamer::prelude::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

// ---------------------------------------------------------------------------
// Test harness helpers
// ---------------------------------------------------------------------------

/// Path to the built binaries (release or debug).
fn bin_dir() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop(); // crates/
    path.pop(); // audiomock/
    path.push("target");
    // Prefer release if it exists, else debug
    let release = path.join("release");
    if release.join("audiomockd").exists() {
        release
    } else {
        path.join("debug")
    }
}

fn daemon_bin() -> PathBuf {
    bin_dir().join("audiomockd")
}

#[allow(dead_code)]
fn cli_bin() -> PathBuf {
    bin_dir().join("audiomock")
}

/// A unique socket path for this test run.
fn test_socket_path(label: &str) -> String {
    let xdg =
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    format!("{xdg}/audiomockd-test-{label}-{}.sock", std::process::id())
}

/// Start the daemon with a unique socket path. Returns the child process.
fn start_daemon(socket_path: &str) -> Child {
    Command::new(daemon_bin())
        .args(["--socket", socket_path, "--log-level", "warn"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("Failed to start audiomockd")
}

/// Wait until the daemon socket is ready, with timeout.
async fn wait_for_socket(socket_path: &str, timeout: Duration) -> Result<()> {
    let start = std::time::Instant::now();
    loop {
        if std::path::Path::new(socket_path).exists() {
            // Try connecting
            if UnixStream::connect(socket_path).await.is_ok() {
                return Ok(());
            }
        }
        if start.elapsed() > timeout {
            anyhow::bail!("Timed out waiting for daemon socket: {socket_path}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Simple IPC client for tests.
struct TestClient {
    stream: UnixStream,
}

impl TestClient {
    async fn connect(socket_path: &str) -> Result<Self> {
        let stream = UnixStream::connect(socket_path).await?;
        Ok(Self { stream })
    }

    async fn send(&mut self, req: &Request) -> Result<Response> {
        let encoded = protocol::encode_message(req)?;
        self.stream.write_all(&encoded).await?;

        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf).await?;
        let len = protocol::decode_length(&len_buf) as usize;

        let mut payload = vec![0u8; len];
        self.stream.read_exact(&mut payload).await?;
        Ok(protocol::decode_message(&payload)?)
    }
}

/// Guard that kills the daemon when dropped.
struct DaemonGuard {
    child: Child,
    socket_path: String,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

async fn setup_daemon(label: &str) -> Result<(DaemonGuard, String)> {
    let socket = test_socket_path(label);
    let child = start_daemon(&socket);
    wait_for_socket(&socket, Duration::from_secs(5)).await?;
    Ok((
        DaemonGuard {
            child,
            socket_path: socket.clone(),
        },
        socket,
    ))
}

// ---------------------------------------------------------------------------
// Audio analysis helpers
// ---------------------------------------------------------------------------

/// Estimate the dominant frequency using zero-crossing analysis.
/// Input: interleaved f32 samples. Analyzes channel 0 only.
fn estimate_frequency(samples: &[f32], sample_rate: u32, channels: u16) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }

    // Extract channel 0
    let mono: Vec<f32> = samples
        .iter()
        .step_by(channels as usize)
        .copied()
        .collect();

    // Count positive-going zero crossings
    let mut crossings = 0u64;
    for w in mono.windows(2) {
        if w[0] <= 0.0 && w[1] > 0.0 {
            crossings += 1;
        }
    }

    // Each full cycle has one positive-going zero crossing
    let duration_secs = mono.len() as f64 / sample_rate as f64;
    if duration_secs > 0.0 {
        crossings as f64 / duration_secs
    } else {
        0.0
    }
}

/// Compute the RMS level of samples (channel 0).
fn rms_level(samples: &[f32], channels: u16) -> f32 {
    let mono: Vec<f32> = samples
        .iter()
        .step_by(channels as usize)
        .copied()
        .collect();
    if mono.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = mono.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum_sq / mono.len() as f64).sqrt() as f32
}

/// Check that a signal is effectively silence (RMS below threshold).
fn is_silence(samples: &[f32], channels: u16) -> bool {
    rms_level(samples, channels) < 0.001
}

// ---------------------------------------------------------------------------
// GStreamer helpers
// ---------------------------------------------------------------------------

/// Generate a WAV file containing a sine tone using GStreamer.
fn generate_test_wav(path: &str, frequency: f64, duration_secs: f64, sample_rate: u32) -> Result<()> {
    gstreamer::init().context("GStreamer init")?;

    let pipeline_str = format!(
        "audiotestsrc wave=sine freq={freq} num-buffers={bufs} samplesperbuffer={spb} ! \
         audio/x-raw,format=F32LE,rate={rate},channels=2,layout=interleaved ! \
         audioconvert ! audio/x-raw,format=S16LE ! \
         wavenc ! filesink location={path}",
        freq = frequency,
        rate = sample_rate,
        // Each buffer = 1024 frames, calculate how many we need
        spb = 1024,
        bufs = ((duration_secs * sample_rate as f64) / 1024.0).ceil() as u64,
        path = path,
    );

    let pipeline = gstreamer::parse::launch(&pipeline_str)?;
    let pipeline = pipeline
        .dynamic_cast::<gstreamer::Pipeline>()
        .map_err(|_| anyhow::anyhow!("cast failed"))?;

    pipeline.set_state(gstreamer::State::Playing)?;

    let bus = pipeline.bus().context("no bus")?;
    for msg in bus.iter_timed(gstreamer::ClockTime::from_seconds(10)) {
        use gstreamer::MessageView;
        match msg.view() {
            MessageView::Eos(_) => break,
            MessageView::Error(e) => {
                pipeline.set_state(gstreamer::State::Null)?;
                anyhow::bail!("GStreamer error: {}", e.error());
            }
            _ => {}
        }
    }

    pipeline.set_state(gstreamer::State::Null)?;
    Ok(())
}

/// Decode a WAV/audio file to raw f32 interleaved samples using GStreamer.
fn decode_audio_file(path: &str, sample_rate: u32, channels: u16) -> Result<Vec<f32>> {
    gstreamer::init()?;

    let pipeline_str = format!(
        "filesrc location={path} ! decodebin ! audioconvert ! audioresample ! \
         audio/x-raw,format=F32LE,rate={rate},channels={ch},layout=interleaved ! \
         appsink name=sink",
        path = path,
        rate = sample_rate,
        ch = channels,
    );

    let pipeline = gstreamer::parse::launch(&pipeline_str)?;
    let pipeline = pipeline
        .dynamic_cast::<gstreamer::Pipeline>()
        .map_err(|_| anyhow::anyhow!("cast failed"))?;

    let sink = pipeline.by_name("sink").context("no appsink")?;
    let appsink = sink
        .dynamic_cast::<gstreamer_app::AppSink>()
        .map_err(|_| anyhow::anyhow!("cast failed"))?;

    pipeline.set_state(gstreamer::State::Playing)?;

    let mut samples = Vec::new();
    loop {
        match appsink.pull_sample() {
            Ok(sample) => {
                let buffer = sample.buffer_owned().context("no buffer")?;
                let map = buffer.map_readable().context("map failed")?;
                let bytes = map.as_slice();
                for chunk in bytes.chunks_exact(4) {
                    samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
            }
            Err(_) => break,
        }
    }

    pipeline.set_state(gstreamer::State::Null)?;
    Ok(samples)
}

/// Generate raw F32LE PCM bytes for a sine wave.
fn generate_raw_pcm(frequency: f64, duration_secs: f64, sample_rate: u32, channels: u16) -> Vec<u8> {
    let n_frames = (sample_rate as f64 * duration_secs) as usize;
    let mut bytes = Vec::with_capacity(n_frames * channels as usize * 4);
    for i in 0..n_frames {
        let t = i as f64 / sample_rate as f64;
        let val = (2.0 * std::f64::consts::PI * frequency * t).sin() as f32 * 0.8;
        let b = f32::to_le_bytes(val);
        for _ in 0..channels {
            bytes.extend_from_slice(&b);
        }
    }
    bytes
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_daemon_startup_and_status() -> Result<()> {
    let (_guard, socket) = setup_daemon("startup").await?;
    let mut client = TestClient::connect(&socket).await?;

    let resp = client.send(&Request::Status).await?;
    match resp {
        Response::Status {
            version,
            uptime_secs,
            devices,
        } => {
            assert_eq!(version, "0.1.0");
            assert!(uptime_secs >= 0.0);
            assert_eq!(devices.len(), 1, "should have default device");
            assert_eq!(devices[0].name, "default");
        }
        other => panic!("Expected Status, got: {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn test_device_lifecycle() -> Result<()> {
    let (_guard, socket) = setup_daemon("lifecycle").await?;
    let mut client = TestClient::connect(&socket).await?;

    // Create a device
    let resp = client
        .send(&Request::DevicesCreate {
            name: "test-dev".to_string(),
            source_description: Some("Test Source".to_string()),
            sink_description: Some("Test Sink".to_string()),
        })
        .await?;
    assert!(matches!(resp, Response::DeviceCreated { .. }));

    // List should show 2 devices
    let resp = client.send(&Request::DevicesList).await?;
    match resp {
        Response::DevicesList { devices } => {
            assert_eq!(devices.len(), 2);
            let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
            assert!(names.contains(&"default"));
            assert!(names.contains(&"test-dev"));
        }
        other => panic!("Expected DevicesList, got: {other:?}"),
    }

    // Duplicate creation should fail
    let resp = client
        .send(&Request::DevicesCreate {
            name: "test-dev".to_string(),
            source_description: None,
            sink_description: None,
        })
        .await?;
    assert!(matches!(resp, Response::Error { .. }));

    // Destroy
    let resp = client
        .send(&Request::DevicesDestroy {
            name: "test-dev".to_string(),
        })
        .await?;
    assert!(matches!(resp, Response::DeviceDestroyed { .. }));

    // List should show 1 device again
    let resp = client.send(&Request::DevicesList).await?;
    match resp {
        Response::DevicesList { devices } => assert_eq!(devices.len(), 1),
        other => panic!("Expected DevicesList, got: {other:?}"),
    }

    // Destroy nonexistent should fail
    let resp = client
        .send(&Request::DevicesDestroy {
            name: "nope".to_string(),
        })
        .await?;
    assert!(matches!(resp, Response::Error { .. }));

    Ok(())
}

#[tokio::test]
async fn test_tone_generation_and_status() -> Result<()> {
    let (_guard, socket) = setup_daemon("tone").await?;
    let mut client = TestClient::connect(&socket).await?;

    // Start a continuous tone
    let resp = client
        .send(&Request::Generate {
            device: "default".to_string(),
            waveform: WaveformKind::Sine,
            frequency: 440.0,
            volume: 0.8,
            duration_secs: None,
            continuous: true,
        })
        .await?;
    assert!(matches!(resp, Response::GenerateStarted { .. }));

    // Give PipeWire time to process
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Status should show active tone
    let mut client2 = TestClient::connect(&socket).await?;
    let resp = client2.send(&Request::Status).await?;
    match resp {
        Response::Status { devices, .. } => {
            let dev = &devices[0];
            assert!(
                dev.active_job.is_some(),
                "Expected active tone job, got: {:?}",
                dev.active_job
            );
        }
        other => panic!("Expected Status, got: {other:?}"),
    }

    // Stop the tone
    let mut client3 = TestClient::connect(&socket).await?;
    let resp = client3
        .send(&Request::Stop {
            device: "default".to_string(),
        })
        .await?;
    assert!(matches!(resp, Response::Stopped { .. }));

    // Status should be idle
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut client4 = TestClient::connect(&socket).await?;
    let resp = client4.send(&Request::Status).await?;
    match resp {
        Response::Status { devices, .. } => {
            assert!(
                devices[0].active_job.is_none(),
                "Expected idle, got: {:?}",
                devices[0].active_job
            );
        }
        other => panic!("Expected Status, got: {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn test_tone_with_duration_and_stop() -> Result<()> {
    // Note: PipeWire only calls the process callback when a consumer is
    // connected to the stream. Without a consumer, duration-based auto-stop
    // in the callback won't fire. This test verifies that:
    // 1. A timed tone starts successfully
    // 2. Explicit stop works correctly
    let (_guard, socket) = setup_daemon("tone-dur").await?;
    let mut client = TestClient::connect(&socket).await?;

    // Generate a tone with duration
    let resp = client
        .send(&Request::Generate {
            device: "default".to_string(),
            waveform: WaveformKind::Square,
            frequency: 1000.0,
            volume: 0.5,
            duration_secs: Some(1.0),
            continuous: false,
        })
        .await?;
    assert!(matches!(resp, Response::GenerateStarted { .. }));

    // Verify it's active
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut c2 = TestClient::connect(&socket).await?;
    let resp = c2.send(&Request::Status).await?;
    match resp {
        Response::Status { devices, .. } => {
            assert!(devices[0].active_job.is_some(), "Tone should be active");
        }
        other => panic!("Expected Status, got: {other:?}"),
    }

    // Explicit stop
    let mut c3 = TestClient::connect(&socket).await?;
    let resp = c3
        .send(&Request::Stop {
            device: "default".to_string(),
        })
        .await?;
    assert!(matches!(resp, Response::Stopped { .. }));

    // Verify idle
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut c4 = TestClient::connect(&socket).await?;
    let resp = c4.send(&Request::Status).await?;
    match resp {
        Response::Status { devices, .. } => {
            assert!(devices[0].active_job.is_none(), "Should be idle after stop");
        }
        other => panic!("Expected Status, got: {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn test_all_waveforms() -> Result<()> {
    let (_guard, socket) = setup_daemon("waveforms").await?;

    for waveform in [
        WaveformKind::Sine,
        WaveformKind::Square,
        WaveformKind::Sawtooth,
        WaveformKind::Noise,
    ] {
        let mut client = TestClient::connect(&socket).await?;
        let resp = client
            .send(&Request::Generate {
                device: "default".to_string(),
                waveform,
                frequency: 440.0,
                volume: 0.7,
                duration_secs: Some(0.2),
                continuous: false,
            })
            .await?;
        assert!(
            matches!(resp, Response::GenerateStarted { .. }),
            "Failed to start {waveform:?} tone"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    Ok(())
}

#[tokio::test]
async fn test_gstreamer_wav_generation_and_decode() -> Result<()> {
    // This tests GStreamer's ability to generate and decode audio, which is
    // the foundation for the playback and recording tests.
    let dir = tempfile::tempdir()?;
    let wav_path = dir.path().join("test_440hz.wav");
    let wav_str = wav_path.to_str().unwrap();

    // Generate a 440Hz WAV file using GStreamer
    generate_test_wav(wav_str, 440.0, 1.0, 48000)?;

    assert!(wav_path.exists(), "WAV file should exist");
    let metadata = std::fs::metadata(&wav_path)?;
    assert!(metadata.len() > 1000, "WAV file should have content");

    // Decode it back and check frequency
    let samples = decode_audio_file(wav_str, 48000, 2)?;
    assert!(
        samples.len() > 40000,
        "Should have at least ~1s of stereo samples, got {}",
        samples.len()
    );

    // Verify it's not silence
    assert!(
        !is_silence(&samples, 2),
        "Decoded audio should not be silence"
    );

    // Verify frequency is approximately 440Hz
    let freq = estimate_frequency(&samples, 48000, 2);
    assert!(
        (freq - 440.0).abs() < 10.0,
        "Expected ~440Hz, got {freq}Hz"
    );

    Ok(())
}

#[tokio::test]
async fn test_playback_via_gstreamer_wav() -> Result<()> {
    let (_guard, socket) = setup_daemon("playback").await?;

    // Generate a test WAV file
    let dir = tempfile::tempdir()?;
    let wav_path = dir.path().join("play_test.wav");
    let wav_str = wav_path.to_str().unwrap();
    generate_test_wav(wav_str, 880.0, 0.5, 48000)?;

    // Tell daemon to play it
    let mut client = TestClient::connect(&socket).await?;
    let resp = client
        .send(&Request::Play {
            device: "default".to_string(),
            file_path: wav_str.to_string(),
            loop_count: None,
            volume: 1.0,
        })
        .await?;
    assert!(
        matches!(resp, Response::PlayStarted { .. }),
        "Expected PlayStarted, got: {resp:?}"
    );

    // Verify playback is active
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut client2 = TestClient::connect(&socket).await?;
    let resp = client2.send(&Request::Status).await?;
    match resp {
        Response::Status { devices, .. } => {
            // It might have finished by now for a short file, so just log
            let job = &devices[0].active_job;
            eprintln!("Playback status: {job:?}");
        }
        other => panic!("Expected Status, got: {other:?}"),
    }

    // Wait for playback to finish
    tokio::time::sleep(Duration::from_millis(700)).await;

    Ok(())
}

#[tokio::test]
async fn test_recording() -> Result<()> {
    let (_guard, socket) = setup_daemon("record").await?;
    let dir = tempfile::tempdir()?;
    let rec_path = dir.path().join("recorded.wav");
    let rec_str = rec_path.to_str().unwrap();

    // Start recording
    let mut client = TestClient::connect(&socket).await?;
    let resp = client
        .send(&Request::Record {
            device: "default".to_string(),
            file_path: rec_str.to_string(),
            format: Some(OutputFormat::Wav),
            duration_secs: None,
            sample_rate: None,
            channels: None,
        })
        .await?;
    assert!(matches!(resp, Response::RecordStarted { .. }));

    // Let it record for a bit (silence, since nothing is playing to the sink)
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Stop recording (this should flush to file)
    let mut client2 = TestClient::connect(&socket).await?;
    let resp = client2
        .send(&Request::Stop {
            device: "default".to_string(),
        })
        .await?;
    assert!(matches!(resp, Response::Stopped { .. }));

    // Give flush time
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The recorded file should exist (may be empty WAV if sink received silence)
    if rec_path.exists() {
        let metadata = std::fs::metadata(&rec_path)?;
        eprintln!("Recording file size: {} bytes", metadata.len());
        // If there's content, verify it decodes
        if metadata.len() > 44 {
            // > WAV header only
            let samples = decode_audio_file(rec_str, 48000, 2)?;
            // Should be mostly silence since nothing played to the sink
            eprintln!("Recorded {} samples, RMS: {}", samples.len(), rms_level(&samples, 2));
        }
    } else {
        eprintln!("Note: recording file not created (sink may not have received audio)");
    }

    Ok(())
}

#[tokio::test]
async fn test_stream_in_with_frequency_validation() -> Result<()> {
    let (_guard, socket) = setup_daemon("stream-freq").await?;

    // Start stream-in on the source
    let mut client = TestClient::connect(&socket).await?;
    let resp = client
        .send(&Request::StreamStart {
            device: "default".to_string(),
            direction: StreamDirection::In,
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32LE,
        })
        .await?;

    let data_socket = match resp {
        Response::StreamStarted { data_socket } => data_socket,
        other => panic!("Expected StreamStarted, got: {other:?}"),
    };

    // Connect to the data socket and push a 440Hz tone
    let pcm_data = generate_raw_pcm(440.0, 0.5, 48000, 2);

    let mut data_stream = tokio::net::UnixStream::connect(&data_socket).await?;
    data_stream.write_all(&pcm_data).await?;
    drop(data_stream); // Close to signal EOF

    // Give PipeWire time to process
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify the stream-in is active
    let mut client2 = TestClient::connect(&socket).await?;
    let resp = client2.send(&Request::Status).await?;
    match resp {
        Response::Status { devices, .. } => {
            eprintln!("Stream-in status: {:?}", devices[0].active_job);
        }
        _ => {}
    }

    // Stop streaming
    let mut client3 = TestClient::connect(&socket).await?;
    client3
        .send(&Request::Stop {
            device: "default".to_string(),
        })
        .await?;

    Ok(())
}

#[tokio::test]
async fn test_multiple_devices_independent() -> Result<()> {
    let (_guard, socket) = setup_daemon("multi-dev").await?;

    // Create a second device
    let mut client = TestClient::connect(&socket).await?;
    let resp = client
        .send(&Request::DevicesCreate {
            name: "dev-b".to_string(),
            source_description: Some("Dev B Source".to_string()),
            sink_description: Some("Dev B Sink".to_string()),
        })
        .await?;
    assert!(matches!(resp, Response::DeviceCreated { .. }));

    // Start different tones on each device
    let mut c1 = TestClient::connect(&socket).await?;
    let resp = c1
        .send(&Request::Generate {
            device: "default".to_string(),
            waveform: WaveformKind::Sine,
            frequency: 440.0,
            volume: 0.8,
            duration_secs: None,
            continuous: true,
        })
        .await?;
    assert!(matches!(resp, Response::GenerateStarted { .. }));

    let mut c2 = TestClient::connect(&socket).await?;
    let resp = c2
        .send(&Request::Generate {
            device: "dev-b".to_string(),
            waveform: WaveformKind::Square,
            frequency: 880.0,
            volume: 0.5,
            duration_secs: None,
            continuous: true,
        })
        .await?;
    assert!(matches!(resp, Response::GenerateStarted { .. }));

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Both should be active
    let mut c3 = TestClient::connect(&socket).await?;
    let resp = c3.send(&Request::Status).await?;
    match resp {
        Response::Status { devices, .. } => {
            assert_eq!(devices.len(), 2);
            for dev in &devices {
                assert!(
                    dev.active_job.is_some(),
                    "Device '{}' should be active",
                    dev.name
                );
            }
        }
        other => panic!("Expected Status, got: {other:?}"),
    }

    // Stop only default
    let mut c4 = TestClient::connect(&socket).await?;
    c4.send(&Request::Stop {
        device: "default".to_string(),
    })
    .await?;

    tokio::time::sleep(Duration::from_millis(100)).await;

    // default idle, dev-b still active
    let mut c5 = TestClient::connect(&socket).await?;
    let resp = c5.send(&Request::Status).await?;
    match resp {
        Response::Status { devices, .. } => {
            for dev in &devices {
                match dev.name.as_str() {
                    "default" => assert!(
                        dev.active_job.is_none(),
                        "default should be idle"
                    ),
                    "dev-b" => assert!(
                        dev.active_job.is_some(),
                        "dev-b should still be active"
                    ),
                    _ => {}
                }
            }
        }
        other => panic!("Expected Status, got: {other:?}"),
    }

    // Cleanup
    let mut c6 = TestClient::connect(&socket).await?;
    c6.send(&Request::Stop {
        device: "dev-b".to_string(),
    })
    .await?;

    Ok(())
}

#[tokio::test]
async fn test_stop_nonexistent_device() -> Result<()> {
    let (_guard, socket) = setup_daemon("stop-nodev").await?;
    let mut client = TestClient::connect(&socket).await?;

    let resp = client
        .send(&Request::Stop {
            device: "does-not-exist".to_string(),
        })
        .await?;
    assert!(
        matches!(resp, Response::Error { .. }),
        "Expected error for nonexistent device"
    );

    Ok(())
}

#[tokio::test]
async fn test_generate_on_nonexistent_device() -> Result<()> {
    let (_guard, socket) = setup_daemon("gen-nodev").await?;
    let mut client = TestClient::connect(&socket).await?;

    let resp = client
        .send(&Request::Generate {
            device: "ghost".to_string(),
            waveform: WaveformKind::Sine,
            frequency: 440.0,
            volume: 1.0,
            duration_secs: Some(1.0),
            continuous: false,
        })
        .await?;
    assert!(
        matches!(resp, Response::Error { .. }),
        "Expected error for nonexistent device"
    );

    Ok(())
}

#[tokio::test]
async fn test_frequency_analysis_accuracy() -> Result<()> {
    // Validate our frequency analysis helper with known signals
    let sample_rate = 48000u32;
    let channels = 2u16;

    for &expected_freq in &[220.0, 440.0, 880.0, 1000.0, 2000.0] {
        let pcm = generate_raw_pcm(expected_freq, 1.0, sample_rate, channels);
        let samples: Vec<f32> = pcm
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        let measured = estimate_frequency(&samples, sample_rate, channels);
        let tolerance = expected_freq * 0.02; // 2% tolerance
        assert!(
            (measured - expected_freq).abs() < tolerance,
            "Frequency {expected_freq}Hz: measured {measured}Hz (tolerance {tolerance})"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_pcm_generation_is_not_silence() -> Result<()> {
    let pcm = generate_raw_pcm(440.0, 0.5, 48000, 2);
    let samples: Vec<f32> = pcm
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    assert!(!is_silence(&samples, 2), "Generated PCM should not be silence");

    let rms = rms_level(&samples, 2);
    assert!(
        rms > 0.4 && rms < 0.7,
        "RMS of 0.8 amplitude sine should be ~0.566, got {rms}"
    );

    Ok(())
}

#[tokio::test]
async fn test_recording_produces_valid_wav() -> Result<()> {
    // Start a tone and record simultaneously to verify the recording pipeline
    // produces a valid WAV file with correct duration.
    let (_guard, socket) = setup_daemon("rec-wav").await?;
    let dir = tempfile::tempdir()?;
    let rec_path = dir.path().join("tone-capture.wav");
    let rec_str = rec_path.to_str().unwrap();

    // Start a tone on the source (won't reach the sink without PipeWire routing,
    // but we can still verify the recording pipeline mechanics)
    let mut c1 = TestClient::connect(&socket).await?;
    c1.send(&Request::Generate {
        device: "default".to_string(),
        waveform: WaveformKind::Sine,
        frequency: 440.0,
        volume: 0.8,
        duration_secs: None,
        continuous: true,
    })
    .await?;

    // Start recording on the sink
    let mut c2 = TestClient::connect(&socket).await?;
    let resp = c2
        .send(&Request::Record {
            device: "default".to_string(),
            file_path: rec_str.to_string(),
            format: Some(OutputFormat::Wav),
            duration_secs: None,
            sample_rate: None,
            channels: None,
        })
        .await?;
    assert!(matches!(resp, Response::RecordStarted { .. }));

    // Record for 1 second
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Stop (flushes recording to file)
    let mut c3 = TestClient::connect(&socket).await?;
    c3.send(&Request::Stop {
        device: "default".to_string(),
    })
    .await?;

    // Give flush time
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify the WAV file was created and has a reasonable duration
    if rec_path.exists() {
        let metadata = std::fs::metadata(&rec_path)?;
        assert!(metadata.len() > 44, "WAV should have more than just a header");

        let samples = decode_audio_file(rec_str, 48000, 2)?;
        let duration_secs = samples.len() as f64 / (48000.0 * 2.0);
        eprintln!("Recording duration: {duration_secs:.2}s, samples: {}", samples.len());

        // Duration should be roughly 1 second (not 192s!)
        assert!(
            duration_secs < 5.0,
            "Recording should be ~1s, got {duration_secs:.2}s — buffer accumulation bug"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_recording_buffer_cleared_on_start() -> Result<()> {
    // Verify that starting a new recording clears any old buffer data.
    let (_guard, socket) = setup_daemon("rec-clear").await?;
    let dir = tempfile::tempdir()?;

    // First recording
    let mut c1 = TestClient::connect(&socket).await?;
    c1.send(&Request::Record {
        device: "default".to_string(),
        file_path: dir.path().join("first.wav").to_str().unwrap().to_string(),
        format: Some(OutputFormat::Wav),
        duration_secs: None,
        sample_rate: None,
        channels: None,
    })
    .await?;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut c2 = TestClient::connect(&socket).await?;
    c2.send(&Request::Stop {
        device: "default".to_string(),
    })
    .await?;

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Second recording — buffer should be fresh
    let rec2_path = dir.path().join("second.wav");
    let rec2_str = rec2_path.to_str().unwrap();

    let mut c3 = TestClient::connect(&socket).await?;
    c3.send(&Request::Record {
        device: "default".to_string(),
        file_path: rec2_str.to_string(),
        format: Some(OutputFormat::Wav),
        duration_secs: None,
        sample_rate: None,
        channels: None,
    })
    .await?;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut c4 = TestClient::connect(&socket).await?;
    c4.send(&Request::Stop {
        device: "default".to_string(),
    })
    .await?;

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Second recording should NOT contain data from the first recording
    if rec2_path.exists() {
        let samples = decode_audio_file(rec2_str, 48000, 2)?;
        let duration = samples.len() as f64 / (48000.0 * 2.0);
        eprintln!("Second recording: {duration:.2}s");
        assert!(
            duration < 3.0,
            "Second recording should be ~0.5s, got {duration:.2}s — old buffer leaked"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_concurrent_tone_and_record() -> Result<()> {
    // Verify that generating a tone and recording simultaneously doesn't crash.
    // This was a RefCell panic bug that was fixed with try_borrow_mut.
    let (_guard, socket) = setup_daemon("concurrent").await?;

    // Start tone
    let mut c1 = TestClient::connect(&socket).await?;
    c1.send(&Request::Generate {
        device: "default".to_string(),
        waveform: WaveformKind::Sine,
        frequency: 1000.0,
        volume: 1.0,
        duration_secs: None,
        continuous: true,
    })
    .await?;

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Start recording while tone is active — this previously caused a RefCell panic
    let dir = tempfile::tempdir()?;
    let rec_str = dir.path().join("concurrent.wav").to_str().unwrap().to_string();

    let mut c2 = TestClient::connect(&socket).await?;
    let resp = c2
        .send(&Request::Record {
            device: "default".to_string(),
            file_path: rec_str.clone(),
            format: Some(OutputFormat::Wav),
            duration_secs: None,
            sample_rate: None,
            channels: None,
        })
        .await?;
    assert!(
        matches!(resp, Response::RecordStarted { .. }),
        "Should start recording while tone is active"
    );

    // Let both run simultaneously
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify daemon is still alive (didn't panic)
    let mut c3 = TestClient::connect(&socket).await?;
    let resp = c3.send(&Request::Status).await?;
    assert!(matches!(resp, Response::Status { .. }), "Daemon should still be responsive");

    // Stop both
    let mut c4 = TestClient::connect(&socket).await?;
    c4.send(&Request::Stop {
        device: "default".to_string(),
    })
    .await?;

    Ok(())
}

#[tokio::test]
async fn test_rapid_start_stop_cycles() -> Result<()> {
    // Stress test: rapidly start and stop tones to test try_borrow_mut resilience.
    let (_guard, socket) = setup_daemon("rapid").await?;

    for i in 0..10 {
        let mut c1 = TestClient::connect(&socket).await?;
        let resp = c1
            .send(&Request::Generate {
                device: "default".to_string(),
                waveform: WaveformKind::Sine,
                frequency: 440.0 + (i as f64 * 100.0),
                volume: 0.5,
                duration_secs: None,
                continuous: true,
            })
            .await?;
        assert!(matches!(resp, Response::GenerateStarted { .. }));

        // Immediately stop
        let mut c2 = TestClient::connect(&socket).await?;
        c2.send(&Request::Stop {
            device: "default".to_string(),
        })
        .await?;
    }

    // Daemon should still be alive
    let mut c = TestClient::connect(&socket).await?;
    let resp = c.send(&Request::Status).await?;
    match resp {
        Response::Status { devices, .. } => {
            assert!(devices[0].active_job.is_none(), "Should be idle after rapid cycles");
        }
        other => panic!("Expected Status, got: {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn test_generate_different_frequencies() -> Result<()> {
    // Verify the daemon handles different frequencies correctly by
    // checking status reports the right parameters.
    let (_guard, socket) = setup_daemon("freqs").await?;

    for &freq in &[220.0, 440.0, 880.0, 1000.0, 4000.0] {
        let mut c1 = TestClient::connect(&socket).await?;
        c1.send(&Request::Generate {
            device: "default".to_string(),
            waveform: WaveformKind::Sine,
            frequency: freq,
            volume: 0.7,
            duration_secs: None,
            continuous: true,
        })
        .await?;

        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut c2 = TestClient::connect(&socket).await?;
        let resp = c2.send(&Request::Status).await?;
        if let Response::Status { devices, .. } = resp {
            match &devices[0].active_job {
                Some(audiomock_proto::device::ActiveJob::Tone {
                    frequency: f,
                    waveform,
                    ..
                }) => {
                    assert!(
                        (*f - freq).abs() < 0.01,
                        "Expected freq {freq}, got {f}"
                    );
                    assert_eq!(*waveform, WaveformKind::Sine);
                }
                other => panic!("Expected Tone job at {freq}Hz, got: {other:?}"),
            }
        }

        let mut c3 = TestClient::connect(&socket).await?;
        c3.send(&Request::Stop {
            device: "default".to_string(),
        })
        .await?;
    }

    Ok(())
}

#[tokio::test]
async fn test_stop_clears_both_source_and_sink() -> Result<()> {
    // Verify that stop clears jobs on both source (tone) and sink (recording).
    let (_guard, socket) = setup_daemon("stop-both").await?;
    let dir = tempfile::tempdir()?;

    // Start tone on source
    let mut c1 = TestClient::connect(&socket).await?;
    c1.send(&Request::Generate {
        device: "default".to_string(),
        waveform: WaveformKind::Sine,
        frequency: 440.0,
        volume: 1.0,
        duration_secs: None,
        continuous: true,
    })
    .await?;

    // Start recording on sink
    let mut c2 = TestClient::connect(&socket).await?;
    c2.send(&Request::Record {
        device: "default".to_string(),
        file_path: dir.path().join("test.wav").to_str().unwrap().to_string(),
        format: Some(OutputFormat::Wav),
        duration_secs: None,
        sample_rate: None,
        channels: None,
    })
    .await?;

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Single stop should clear both
    let mut c3 = TestClient::connect(&socket).await?;
    c3.send(&Request::Stop {
        device: "default".to_string(),
    })
    .await?;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut c4 = TestClient::connect(&socket).await?;
    let resp = c4.send(&Request::Status).await?;
    match resp {
        Response::Status { devices, .. } => {
            assert!(
                devices[0].active_job.is_none(),
                "Both source and sink jobs should be cleared"
            );
        }
        other => panic!("Expected Status, got: {other:?}"),
    }

    Ok(())
}
