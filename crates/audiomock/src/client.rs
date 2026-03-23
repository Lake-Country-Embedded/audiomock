use anyhow::{Context, Result};
use audiomock_proto::protocol::{self, Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

pub struct Client {
    stream: UnixStream,
}

impl Client {
    pub async fn connect(socket_path: &str) -> Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to connect to daemon at {socket_path}. Is audiomockd running?"
                )
            })?;
        Ok(Self { stream })
    }

    pub async fn send(&mut self, request: &Request) -> Result<Response> {
        let encoded =
            protocol::encode_message(request).context("Failed to encode request")?;
        self.stream.write_all(&encoded).await?;

        // Read response
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf).await?;
        let len = protocol::decode_length(&len_buf) as usize;

        let mut payload = vec![0u8; len];
        self.stream.read_exact(&mut payload).await?;

        let response: Response =
            protocol::decode_message(&payload).context("Failed to decode response")?;
        Ok(response)
    }
}
