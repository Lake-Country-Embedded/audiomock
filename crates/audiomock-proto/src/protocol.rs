use serde::{Deserialize, Serialize};

use crate::audio::{OutputFormat, SampleFormat, WaveformKind};
use crate::device::{DeviceInfo, StreamDirection};

/// Requests sent from CLI to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    /// Query daemon status.
    Status,

    /// List all device pairs.
    DevicesList,

    /// Create a new device pair.
    DevicesCreate {
        name: String,
        source_description: Option<String>,
        sink_description: Option<String>,
    },

    /// Destroy a device pair.
    DevicesDestroy { name: String },

    /// Generate a tone on a virtual source.
    Generate {
        device: String,
        waveform: WaveformKind,
        frequency: f64,
        volume: f32,
        duration_secs: Option<f64>,
        continuous: bool,
    },

    /// Play an audio file to a virtual source.
    Play {
        device: String,
        file_path: String,
        loop_count: Option<u32>,
        volume: f32,
    },

    /// Record from a virtual sink to a file.
    Record {
        device: String,
        file_path: String,
        format: Option<OutputFormat>,
        duration_secs: Option<f64>,
        sample_rate: Option<u32>,
        channels: Option<u16>,
    },

    /// Start raw PCM streaming.
    StreamStart {
        device: String,
        direction: StreamDirection,
        sample_rate: u32,
        channels: u16,
        sample_format: SampleFormat,
    },

    /// Stop any active job on a device.
    Stop { device: String },
}

/// Responses sent from daemon to CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    /// Daemon status info.
    Status {
        version: String,
        uptime_secs: f64,
        devices: Vec<DeviceInfo>,
    },

    /// List of device pairs.
    DevicesList { devices: Vec<DeviceInfo> },

    /// Device created successfully.
    DeviceCreated { name: String },

    /// Device destroyed successfully.
    DeviceDestroyed { name: String },

    /// Tone generation started.
    GenerateStarted { device: String },

    /// Playback started.
    PlayStarted { device: String },

    /// Recording started.
    RecordStarted { device: String },

    /// Streaming started, includes the data socket path.
    StreamStarted { data_socket: String },

    /// Job stopped.
    Stopped { device: String },

    /// Operation completed (playback finished, recording finished, etc.).
    Completed { device: String, message: String },

    /// Progress update.
    Progress {
        device: String,
        elapsed_secs: f64,
        total_secs: Option<f64>,
    },

    /// Error response.
    Error { message: String },
}

/// Frame a message for sending over the wire.
/// Format: 4-byte big-endian length prefix + JSON payload.
pub fn encode_message<T: Serialize>(msg: &T) -> Result<Vec<u8>, serde_json::Error> {
    let json = serde_json::to_vec(msg)?;
    let len = (json.len() as u32).to_be_bytes();
    let mut buf = Vec::with_capacity(4 + json.len());
    buf.extend_from_slice(&len);
    buf.extend_from_slice(&json);
    Ok(buf)
}

/// Read the length prefix from a 4-byte buffer.
pub fn decode_length(header: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*header)
}

/// Deserialize a message from a JSON payload.
pub fn decode_message<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_request() {
        let req = Request::Generate {
            device: "test".to_string(),
            waveform: WaveformKind::Sine,
            frequency: 440.0,
            volume: 0.8,
            duration_secs: Some(5.0),
            continuous: false,
        };
        let encoded = encode_message(&req).unwrap();
        let len = decode_length(&encoded[..4].try_into().unwrap()) as usize;
        let decoded: Request = decode_message(&encoded[4..4 + len]).unwrap();
        if let Request::Generate {
            frequency,
            waveform,
            ..
        } = decoded
        {
            assert!((frequency - 440.0).abs() < f64::EPSILON);
            assert_eq!(waveform, WaveformKind::Sine);
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn roundtrip_response() {
        let resp = Response::Error {
            message: "not found".to_string(),
        };
        let encoded = encode_message(&resp).unwrap();
        let len = decode_length(&encoded[..4].try_into().unwrap()) as usize;
        let decoded: Response = decode_message(&encoded[4..4 + len]).unwrap();
        if let Response::Error { message } = decoded {
            assert_eq!(message, "not found");
        } else {
            panic!("unexpected variant");
        }
    }
}
