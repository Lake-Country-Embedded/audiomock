use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use audiomock_proto::protocol::{self, Request};

use crate::daemon::DaemonState;

use super::handler;

pub async fn run(state: Arc<Mutex<DaemonState>>, socket_path: &str) -> Result<()> {
    // Remove stale socket file
    let _ = std::fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("Failed to bind to {socket_path}"))?;

    tracing::info!("IPC server listening on {socket_path}");

    // Handle graceful shutdown
    let state_clone = state.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Shutting down...");
        let st = state_clone.lock().await;
        let _ = st.pw_handle.send(crate::pipewire::PwCommand::Shutdown);
        // Clean up socket
        std::process::exit(0);
    });

    loop {
        let (stream, _addr) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(state, stream).await {
                tracing::error!("Connection error: {e}");
            }
        });
    }
}

async fn handle_connection(
    state: Arc<Mutex<DaemonState>>,
    mut stream: UnixStream,
) -> Result<()> {
    loop {
        // Read length prefix
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(()); // Client disconnected
            }
            Err(e) => return Err(e.into()),
        }

        let len = protocol::decode_length(&len_buf) as usize;
        if len > 1024 * 1024 {
            anyhow::bail!("Message too large: {len} bytes");
        }

        // Read payload
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload).await?;

        let request: Request = protocol::decode_message(&payload)
            .context("Failed to decode request")?;

        tracing::debug!("Received request: {request:?}");

        let response = handler::handle_request(&state, request).await;

        tracing::debug!("Sending response: {response:?}");

        let encoded = protocol::encode_message(&response)
            .context("Failed to encode response")?;
        stream.write_all(&encoded).await?;
    }
}
