use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SampleFormat {
    S16LE,
    S32LE,
    F32LE,
}

impl SampleFormat {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            SampleFormat::S16LE => 2,
            SampleFormat::S32LE | SampleFormat::F32LE => 4,
        }
    }
}

impl std::fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SampleFormat::S16LE => write!(f, "S16LE"),
            SampleFormat::S32LE => write!(f, "S32LE"),
            SampleFormat::F32LE => write!(f, "F32LE"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: SampleFormat,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32LE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaveformKind {
    Sine,
    Square,
    Sawtooth,
    Noise,
}

impl std::fmt::Display for WaveformKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WaveformKind::Sine => write!(f, "sine"),
            WaveformKind::Square => write!(f, "square"),
            WaveformKind::Sawtooth => write!(f, "sawtooth"),
            WaveformKind::Noise => write!(f, "noise"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputFormat {
    Wav,
    Flac,
    Ogg,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputFormat::Wav => write!(f, "wav"),
            OutputFormat::Flac => write!(f, "flac"),
            OutputFormat::Ogg => write!(f, "ogg"),
        }
    }
}

impl OutputFormat {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "wav" => Some(OutputFormat::Wav),
            "flac" => Some(OutputFormat::Flac),
            "ogg" | "oga" => Some(OutputFormat::Ogg),
            _ => None,
        }
    }
}
