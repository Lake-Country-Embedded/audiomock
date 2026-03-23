mod audio;
mod daemon;
mod ipc;
mod pipewire;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "audiomockd", about = "Virtual audio device daemon for QEMU testing")]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "config.toml")]
    config: String,

    /// Override IPC socket path
    #[arg(short, long)]
    socket: Option<String>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long)]
    log_level: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let mut config = match audiomock_proto::config::load_config(&args.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Warning: could not load config '{}': {e}. Using defaults.", args.config);
            audiomock_proto::config::DaemonConfig::default()
        }
    };

    if let Some(socket) = args.socket {
        config.daemon.socket_path = Some(socket);
    }
    if let Some(level) = args.log_level {
        config.daemon.log_level = level;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| config.daemon.log_level.parse().unwrap_or_default()),
        )
        .init();

    tracing::info!("audiomockd starting");

    daemon::run(config).await
}
