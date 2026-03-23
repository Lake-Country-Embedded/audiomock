use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default)]
    pub daemon: DaemonSection,
    #[serde(default)]
    pub audio: AudioSection,
    #[serde(default)]
    pub device_pairs: Vec<DevicePairConfig>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            daemon: DaemonSection::default(),
            audio: AudioSection::default(),
            device_pairs: vec![DevicePairConfig {
                name: "default".to_string(),
                source_description: Some("Virtual Mic".to_string()),
                sink_description: Some("Virtual Speaker".to_string()),
                auto_link_pattern: None,
            }],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonSection {
    pub socket_path: Option<String>,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for DaemonSection {
    fn default() -> Self {
        Self {
            socket_path: None,
            log_level: default_log_level(),
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSection {
    #[serde(default = "default_sample_rate")]
    pub default_sample_rate: u32,
    #[serde(default = "default_channels")]
    pub default_channels: u16,
    #[serde(default = "default_sample_format")]
    pub default_sample_format: String,
    #[serde(default = "default_buffer_size")]
    pub buffer_size: u32,
}

impl Default for AudioSection {
    fn default() -> Self {
        Self {
            default_sample_rate: default_sample_rate(),
            default_channels: default_channels(),
            default_sample_format: default_sample_format(),
            buffer_size: default_buffer_size(),
        }
    }
}

fn default_sample_rate() -> u32 {
    48000
}
fn default_channels() -> u16 {
    2
}
fn default_sample_format() -> String {
    "F32LE".to_string()
}
fn default_buffer_size() -> u32 {
    1024
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePairConfig {
    pub name: String,
    pub source_description: Option<String>,
    pub sink_description: Option<String>,
    pub auto_link_pattern: Option<String>,
}

impl DaemonConfig {
    pub fn socket_path(&self) -> String {
        if let Some(ref path) = self.daemon.socket_path {
            return path.clone();
        }
        if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
            format!("{xdg}/audiomockd.sock")
        } else {
            "/tmp/audiomockd.sock".to_string()
        }
    }
}

pub fn default_socket_path() -> String {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        format!("{xdg}/audiomockd.sock")
    } else {
        "/tmp/audiomockd.sock".to_string()
    }
}

pub fn load_config(path: &str) -> Result<DaemonConfig, Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(path)?;
    let config: DaemonConfig = toml::from_str(&contents)?;
    Ok(config)
}
