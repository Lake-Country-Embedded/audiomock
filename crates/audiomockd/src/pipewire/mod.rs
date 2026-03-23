pub mod context;
pub mod format;
pub mod virtual_node;

use anyhow::Result;
use audiomock_proto::audio::{SampleFormat, WaveformKind};
use audiomock_proto::config::DaemonConfig;
use audiomock_proto::device::{DeviceInfo, StreamDirection};
use crossbeam_channel::Sender;

/// Commands sent from the IPC layer to the PipeWire thread.
#[derive(Debug)]
pub enum PwCommand {
    CreateDevice {
        name: String,
        source_description: String,
        sink_description: String,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    DestroyDevice {
        name: String,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    ListDevices {
        reply: tokio::sync::oneshot::Sender<Vec<DeviceInfo>>,
    },
    StartTone {
        device: String,
        waveform: WaveformKind,
        frequency: f64,
        volume: f32,
        duration_secs: Option<f64>,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    StopJob {
        device: String,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    PlayFile {
        device: String,
        file_path: String,
        loop_count: Option<u32>,
        volume: f32,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    StartRecord {
        device: String,
        file_path: String,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    StartStream {
        device: String,
        direction: StreamDirection,
        /// Reply with the ring buffer for the IPC layer to use, or an error.
        reply: tokio::sync::oneshot::Sender<Result<std::sync::Arc<crate::audio::ring_buffer::RingBuffer>, String>>,
    },
    Shutdown,
}

/// Handle to communicate with the PipeWire thread.
pub struct PipewireHandle {
    pub cmd_tx: Sender<PwCommand>,
    pub thread: Option<std::thread::JoinHandle<()>>,
}

impl PipewireHandle {
    pub fn send(&self, cmd: PwCommand) -> Result<()> {
        self.cmd_tx.send(cmd)?;
        Ok(())
    }
}

pub fn start(config: &DaemonConfig) -> Result<PipewireHandle> {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let device_configs = config.device_pairs.clone();
    let audio_config = config.audio.clone();

    let thread = std::thread::Builder::new()
        .name("pipewire".to_string())
        .spawn(move || {
            if let Err(e) = context::run_pipewire_loop(cmd_rx, device_configs, audio_config) {
                tracing::error!("PipeWire thread error: {e}");
            }
        })?;

    Ok(PipewireHandle {
        cmd_tx,
        thread: Some(thread),
    })
}
