use anyhow::Result;
use audiomock_proto::protocol::{Request, Response};

use crate::client::Client;

/// Link a device pair to peer nodes matching a pattern.
///
/// The graph work happens in the daemon so that `auto_link_pattern` and this
/// command share one implementation.
pub async fn run(
    socket_path: &str,
    device: String,
    pattern: Option<String>,
    exclusive: bool,
    json_output: bool,
) -> Result<()> {
    let mut client = Client::connect(socket_path).await?;
    let response = client
        .send(&Request::Link {
            device,
            pattern,
            exclusive,
        })
        .await?;
    report(response, json_output)
}

/// Remove every link touching a device pair.
pub async fn unlink(socket_path: &str, device: String, json_output: bool) -> Result<()> {
    let mut client = Client::connect(socket_path).await?;
    let response = client.send(&Request::Unlink { device }).await?;
    report(response, json_output)
}

fn report(response: Response, json_output: bool) -> Result<()> {
    match response {
        Response::Linked {
            device,
            created,
            removed,
        } => {
            if json_output {
                println!(
                    "{}",
                    serde_json::json!({
                        "device": device,
                        "created": created,
                        "removed": removed,
                    })
                );
            } else {
                println!("Device '{device}': {created} link(s) created, {removed} removed.");
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
