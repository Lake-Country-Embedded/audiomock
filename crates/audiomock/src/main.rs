mod client;
mod commands;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "audiomock", about = "CLI tool for audiomockd virtual audio devices")]
struct Cli {
    /// Override daemon socket path
    #[arg(long)]
    socket: Option<String>,

    /// Output in JSON format
    #[arg(long)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Manage virtual device pairs
    Devices {
        #[command(subcommand)]
        action: DeviceAction,
    },

    /// Play an audio file to a virtual source
    Play {
        /// Path to audio file (WAV, MP3, FLAC, OGG)
        file: String,

        /// Target device pair
        #[arg(long, default_value = "default")]
        device: String,

        /// Loop count (0 = infinite)
        #[arg(long, name = "loop")]
        loop_count: Option<u32>,

        /// Playback volume (0.0 - 1.0)
        #[arg(long, default_value = "1.0")]
        volume: f32,
    },

    /// Generate a test tone
    Generate {
        /// Target device pair
        #[arg(long, default_value = "default")]
        device: String,

        /// Waveform type
        #[arg(long, default_value = "sine")]
        waveform: String,

        /// Tone frequency in Hz
        #[arg(long, default_value = "440")]
        frequency: f64,

        /// Duration in seconds (omit for indefinite)
        #[arg(long)]
        duration: Option<f64>,

        /// Volume (0.0 - 1.0)
        #[arg(long, default_value = "1.0")]
        volume: f32,

        /// Run in background on the daemon (detach from CLI)
        #[arg(long)]
        continuous: bool,
    },

    /// Record audio from a virtual sink
    Record {
        /// Output file path
        file: String,

        /// Target device pair
        #[arg(long, default_value = "default")]
        device: String,

        /// Output format (wav, flac, ogg). Default: inferred from extension
        #[arg(long)]
        format: Option<String>,

        /// Recording duration in seconds (omit for indefinite, Ctrl+C to stop)
        #[arg(long)]
        duration: Option<f64>,

        /// Output sample rate
        #[arg(long)]
        sample_rate: Option<u32>,

        /// Output channel count
        #[arg(long)]
        channels: Option<u16>,
    },

    /// Raw PCM streaming
    Stream {
        /// Target device pair
        #[arg(long, default_value = "default")]
        device: String,

        /// Direction: in (stdin PCM -> source) or out (sink PCM -> stdout)
        #[arg(long, default_value = "in")]
        direction: String,

        /// Sample rate
        #[arg(long, default_value = "48000")]
        sample_rate: u32,

        /// Channel count
        #[arg(long, default_value = "2")]
        channels: u16,

        /// Sample format: s16le or f32le
        #[arg(long, default_value = "f32le")]
        format: String,
    },

    /// Link QEMU PipeWire nodes to a virtual device pair
    Link {
        /// Target device pair
        #[arg(long, default_value = "default")]
        device: String,

        /// QEMU node name pattern to match (default: "QEMU")
        #[arg(long)]
        qemu_node: Option<String>,
    },

    /// Stop active playback/tone on a device
    Stop {
        /// Target device pair
        #[arg(long, default_value = "default")]
        device: String,
    },

    /// Show daemon status
    Status,
}

#[derive(Subcommand, Debug)]
enum DeviceAction {
    /// List all device pairs
    List,
    /// Create a new device pair
    Create {
        name: String,
        #[arg(long)]
        source_description: Option<String>,
        #[arg(long)]
        sink_description: Option<String>,
    },
    /// Destroy a device pair
    Destroy { name: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let socket_path = cli
        .socket
        .unwrap_or_else(audiomock_proto::config::default_socket_path);

    let json_output = cli.json;

    match cli.command {
        Commands::Status => {
            commands::status::run(&socket_path, json_output).await
        }
        Commands::Devices { action } => match action {
            DeviceAction::List => {
                commands::devices::list(&socket_path, json_output).await
            }
            DeviceAction::Create {
                name,
                source_description,
                sink_description,
            } => {
                commands::devices::create(&socket_path, name, source_description, sink_description)
                    .await
            }
            DeviceAction::Destroy { name } => {
                commands::devices::destroy(&socket_path, name).await
            }
        },
        Commands::Play {
            file,
            device,
            loop_count,
            volume,
        } => commands::play::run(&socket_path, device, file, loop_count, volume).await,
        Commands::Generate {
            device,
            waveform,
            frequency,
            duration,
            volume,
            continuous,
        } => {
            commands::generate::run(
                &socket_path,
                device,
                waveform,
                frequency,
                duration,
                volume,
                continuous,
            )
            .await
        }
        Commands::Record {
            file,
            device,
            format,
            duration,
            sample_rate,
            channels,
        } => {
            commands::record::run(
                &socket_path,
                device,
                file,
                format,
                duration,
                sample_rate,
                channels,
            )
            .await
        }
        Commands::Link {
            device,
            qemu_node,
        } => commands::link::run(&device, qemu_node.as_deref()).await,
        Commands::Stream {
            device,
            direction,
            sample_rate,
            channels,
            format,
        } => {
            commands::stream::run(&socket_path, device, direction, sample_rate, channels, format)
                .await
        }
        Commands::Stop { device } => {
            commands::stop::run(&socket_path, device).await
        }
    }
}
