use anyhow::Result;
use audiomock_proto::audio::OutputFormat;
use audiomock_proto::protocol::{Request, Response};

use crate::client::Client;

pub async fn run(
    socket_path: &str,
    device: String,
    file_path: String,
    format: Option<String>,
    duration: Option<f64>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
) -> Result<()> {
    let output_format = if let Some(ref fmt) = format {
        Some(match fmt.to_lowercase().as_str() {
            "wav" => OutputFormat::Wav,
            "flac" => OutputFormat::Flac,
            "ogg" => OutputFormat::Ogg,
            other => {
                eprintln!("Unknown format: {other}. Use: wav, flac, ogg");
                std::process::exit(1);
            }
        })
    } else {
        // Infer from extension
        std::path::Path::new(&file_path)
            .extension()
            .and_then(|e| e.to_str())
            .and_then(OutputFormat::from_extension)
    };

    let mut client = Client::connect(socket_path).await?;
    let response = client
        .send(&Request::Record {
            device: device.clone(),
            file_path: file_path.clone(),
            format: output_format,
            duration_secs: duration,
            sample_rate,
            channels,
        })
        .await?;

    match response {
        Response::RecordStarted { device } => {
            println!("Recording from '{device}' to '{file_path}'");
            if let Some(d) = duration {
                println!("Duration: {d}s");
                tokio::time::sleep(std::time::Duration::from_secs_f64(d)).await;
                let mut client = Client::connect(socket_path).await?;
                client.send(&Request::Stop { device }).await?;
                println!("Recording complete.");
            } else {
                println!("Press Ctrl+C to stop recording.");
                tokio::signal::ctrl_c().await?;
                let mut client = Client::connect(socket_path).await?;
                client.send(&Request::Stop { device }).await?;
                println!("\nRecording saved.");
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
