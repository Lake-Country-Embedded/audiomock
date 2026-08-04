use anyhow::Result;
use audiomock_proto::protocol::{Request, Response};

use crate::client::Client;

pub async fn run(socket_path: &str, device: String, json_output: bool) -> Result<()> {
    let mut client = Client::connect(socket_path).await?;
    let response = client.send(&Request::Loopback { device }).await?;

    match response {
        Response::LoopbackStarted { device } => {
            if json_output {
                println!(
                    "{}",
                    serde_json::json!({ "device": device, "loopback": true })
                );
            } else {
                println!(
                    "Loopback active on '{device}': sink audio is routed back into the source."
                );
                println!("Stop it with: audiomock stop --device {device}");
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
