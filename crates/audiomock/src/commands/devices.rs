use anyhow::Result;
use audiomock_proto::protocol::{Request, Response};

use crate::client::Client;

pub async fn list(socket_path: &str, json_output: bool) -> Result<()> {
    let mut client = Client::connect(socket_path).await?;
    let response = client.send(&Request::DevicesList).await?;

    match response {
        Response::DevicesList { devices } => {
            if json_output {
                println!("{}", serde_json::to_string_pretty(&devices)?);
            } else if devices.is_empty() {
                println!("No device pairs configured.");
            } else {
                for dev in &devices {
                    let status = match &dev.active_job {
                        Some(job) => format!("{job:?}"),
                        None => "idle".to_string(),
                    };
                    println!(
                        "{:<20} source: {:<30} sink: {:<30} [{}]",
                        dev.name, dev.source_description, dev.sink_description, status
                    );
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

pub async fn create(
    socket_path: &str,
    name: String,
    source_description: Option<String>,
    sink_description: Option<String>,
) -> Result<()> {
    let mut client = Client::connect(socket_path).await?;
    let response = client
        .send(&Request::DevicesCreate {
            name,
            source_description,
            sink_description,
        })
        .await?;

    match response {
        Response::DeviceCreated { name } => {
            println!("Created device pair: {name}");
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

pub async fn destroy(socket_path: &str, name: String) -> Result<()> {
    let mut client = Client::connect(socket_path).await?;
    let response = client.send(&Request::DevicesDestroy { name }).await?;

    match response {
        Response::DeviceDestroyed { name } => {
            println!("Destroyed device pair: {name}");
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
