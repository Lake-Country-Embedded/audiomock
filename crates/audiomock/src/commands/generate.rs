use anyhow::{bail, Result};
use audiomock_proto::audio::WaveformKind;
use audiomock_proto::protocol::{Request, Response};

use crate::client::Client;

pub async fn run(
    socket_path: &str,
    device: String,
    waveform: String,
    frequency: f64,
    duration: Option<f64>,
    volume: f32,
    continuous: bool,
) -> Result<()> {
    let waveform = match waveform.to_lowercase().as_str() {
        "sine" => WaveformKind::Sine,
        "square" => WaveformKind::Square,
        "sawtooth" | "saw" => WaveformKind::Sawtooth,
        "noise" | "white" => WaveformKind::Noise,
        other => bail!("Unknown waveform: {other}. Use: sine, square, sawtooth, noise"),
    };

    let mut client = Client::connect(socket_path).await?;
    let response = client
        .send(&Request::Generate {
            device: device.clone(),
            waveform,
            frequency,
            volume,
            duration_secs: duration,
            continuous,
        })
        .await?;

    match response {
        Response::GenerateStarted { device } => {
            println!(
                "Generating {waveform} tone at {frequency}Hz on '{device}'{}",
                if continuous {
                    " (continuous)"
                } else {
                    ""
                }
            );

            if !continuous && duration.is_none() {
                println!("Press Ctrl+C to stop.");
                // Wait for Ctrl+C, then send stop
                tokio::signal::ctrl_c().await?;
                let mut client = Client::connect(socket_path).await?;
                client.send(&Request::Stop { device }).await?;
                println!("\nStopped.");
            } else if !continuous {
                if let Some(d) = duration {
                    println!("Playing for {d}s...");
                    tokio::time::sleep(std::time::Duration::from_secs_f64(d)).await;
                    println!("Done.");
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
