use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use audiomock_proto::config::DaemonConfig;
use tokio::sync::Mutex;

use crate::ipc;
use crate::pipewire as pw_backend;

pub struct DaemonState {
    pub config: DaemonConfig,
    pub start_time: Instant,
    pub pw_handle: pw_backend::PipewireHandle,
}

pub async fn run(config: DaemonConfig) -> Result<()> {
    let socket_path = config.socket_path();

    // Start PipeWire backend on a dedicated thread
    let pw_handle = pw_backend::start(&config)?;

    let state = Arc::new(Mutex::new(DaemonState {
        config,
        start_time: Instant::now(),
        pw_handle,
    }));

    tracing::info!("Listening on {socket_path}");

    // Run IPC server (blocks until shutdown)
    ipc::server::run(state, &socket_path).await
}
