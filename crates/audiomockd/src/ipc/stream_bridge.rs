use std::sync::Arc;

use anyhow::{Context, Result};
use audiomock_proto::device::StreamDirection;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

use crate::audio::ring_buffer::RingBuffer;

const CHUNK_SIZE: usize = 4096; // bytes per read/write chunk

/// Run a data socket bridge between a Unix socket and a ring buffer.
/// For stream-in: reads bytes from socket, converts to f32, writes to ring buffer.
/// For stream-out: reads from ring buffer, converts to bytes, writes to socket.
pub async fn run(
    socket_path: &str,
    ring_buffer: Arc<RingBuffer>,
    direction: StreamDirection,
) -> Result<()> {
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("Failed to bind data socket: {socket_path}"))?;

    tracing::debug!("Stream bridge listening on {socket_path}");

    // Accept one connection
    let (mut stream, _) = listener.accept().await?;
    tracing::debug!("Stream bridge client connected");

    match direction {
        StreamDirection::In => {
            // Read raw f32 PCM from the socket, write to ring buffer
            let mut buf = vec![0u8; CHUNK_SIZE];
            loop {
                let n = match stream.read(&mut buf).await {
                    Ok(0) => break, // EOF
                    Ok(n) => n,
                    Err(e) => {
                        tracing::debug!("Stream-in read error: {e}");
                        break;
                    }
                };

                // Convert bytes to f32 samples
                let n_floats = n / 4;
                let mut samples = Vec::with_capacity(n_floats);
                for i in 0..n_floats {
                    let offset = i * 4;
                    if offset + 4 <= n {
                        let val = f32::from_le_bytes([
                            buf[offset],
                            buf[offset + 1],
                            buf[offset + 2],
                            buf[offset + 3],
                        ]);
                        samples.push(val);
                    }
                }

                ring_buffer.write(&samples);
            }
        }
        StreamDirection::Out => {
            // Read from ring buffer, write raw f32 PCM to the socket
            let mut temp = vec![0.0f32; CHUNK_SIZE / 4];
            loop {
                let read = ring_buffer.read(&mut temp);
                if read == 0 {
                    // No data available, wait briefly
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    continue;
                }

                let mut bytes = Vec::with_capacity(read * 4);
                for &sample in &temp[..read] {
                    bytes.extend_from_slice(&f32::to_le_bytes(sample));
                }

                if let Err(e) = stream.write_all(&bytes).await {
                    tracing::debug!("Stream-out write error: {e}");
                    break;
                }
            }
        }
    }

    tracing::debug!("Stream bridge finished");
    Ok(())
}
