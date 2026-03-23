use anyhow::Result;
use audiomock_proto::protocol::{Request, Response};

use crate::client::Client;

pub async fn run(
    socket_path: &str,
    device: String,
    file_path: String,
    loop_count: Option<u32>,
    volume: f32,
) -> Result<()> {
    let mut client = Client::connect(socket_path).await?;
    let response = client
        .send(&Request::Play {
            device: device.clone(),
            file_path: file_path.clone(),
            loop_count,
            volume,
        })
        .await?;

    match response {
        Response::PlayStarted { device } => {
            println!("Playing '{file_path}' on '{device}'");
            if loop_count == Some(0) {
                println!("Looping indefinitely. Press Ctrl+C to stop.");
                tokio::signal::ctrl_c().await?;
                let mut client = Client::connect(socket_path).await?;
                client.send(&Request::Stop { device }).await?;
                println!("\nStopped.");
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
