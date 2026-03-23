use serde::{Deserialize, Serialize};

use crate::audio::{AudioFormat, WaveformKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub source_description: String,
    pub sink_description: String,
    pub format: AudioFormat,
    pub active_job: Option<ActiveJob>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActiveJob {
    Tone {
        waveform: WaveformKind,
        frequency: f64,
        volume: f32,
    },
    Playback {
        file_path: String,
        looping: bool,
        volume: f32,
    },
    Recording {
        file_path: String,
    },
    Streaming {
        direction: StreamDirection,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamDirection {
    In,
    Out,
}
