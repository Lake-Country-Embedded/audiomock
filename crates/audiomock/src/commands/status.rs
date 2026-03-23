use anyhow::Result;
use audiomock_proto::protocol::{Request, Response};

use crate::client::Client;

pub async fn run(socket_path: &str, json_output: bool) -> Result<()> {
    let mut client = Client::connect(socket_path).await?;
    let response = client.send(&Request::Status).await?;

    match response {
        Response::Status {
            version,
            uptime_secs,
            devices,
        } => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "version": version,
                        "uptime_secs": uptime_secs,
                        "devices": devices,
                    }))?
                );
            } else {
                println!("audiomockd v{version}");
                println!("Uptime: {:.1}s", uptime_secs);
                println!("Device pairs: {}", devices.len());
                for dev in &devices {
                    println!("  {} (source: {}, sink: {})", dev.name, dev.source_description, dev.sink_description);
                    if let Some(ref job) = dev.active_job {
                        println!("    Active: {job:?}");
                    }
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
