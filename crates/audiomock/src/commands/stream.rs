use anyhow::{bail, Result};
use audiomock_proto::audio::SampleFormat;
use audiomock_proto::device::StreamDirection;
use audiomock_proto::protocol::{Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::client::Client;

pub async fn run(
    socket_path: &str,
    device: String,
    direction: String,
    sample_rate: u32,
    channels: u16,
    format: String,
) -> Result<()> {
    let direction = match direction.to_lowercase().as_str() {
        "in" => StreamDirection::In,
        "out" => StreamDirection::Out,
        other => bail!("Unknown direction: {other}. Use: in, out"),
    };

    let sample_format = match format.to_lowercase().as_str() {
        "s16le" => SampleFormat::S16LE,
        "f32le" => SampleFormat::F32LE,
        other => bail!("Unknown format: {other}. Use: s16le, f32le"),
    };

    let mut client = Client::connect(socket_path).await?;
    let response = client
        .send(&Request::StreamStart {
            device: device.clone(),
            direction,
            sample_rate,
            channels,
            sample_format,
        })
        .await?;

    match response {
        Response::StreamStarted { data_socket } => {
            eprintln!("Streaming on '{device}' via {data_socket}");
            // Connect to data socket for raw PCM transfer
            let mut data_stream = tokio::net::UnixStream::connect(&data_socket).await?;

            match direction {
                StreamDirection::In => {
                    // Pipe stdin to the data socket
                    let mut stdin = tokio::io::stdin();
                    tokio::io::copy(&mut stdin, &mut data_stream).await?;
                }
                StreamDirection::Out => {
                    // Pipe data socket to stdout
                    let mut stdout = tokio::io::stdout();
                    tokio::io::copy(&mut data_stream, &mut stdout).await?;
                }
            }
        }
        Response::Error { message } => {
            eprintln!("Error: {message}");
            std::process::exit(1);
        }
        _ => {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }
    }

    Ok(())
}
