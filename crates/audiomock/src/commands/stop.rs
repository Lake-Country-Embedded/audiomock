use anyhow::Result;
use audiomock_proto::protocol::{Request, Response};

use crate::client::Client;

pub async fn run(socket_path: &str, device: String) -> Result<()> {
    let mut client = Client::connect(socket_path).await?;
    let response = client.send(&Request::Stop { device }).await?;

    match response {
        Response::Stopped { device } => {
            println!("Stopped active job on '{device}'");
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
