use std::sync::Arc;

use audiomock_proto::protocol::{Request, Response};
use tokio::sync::Mutex;

use crate::daemon::DaemonState;
use crate::pipewire::PwCommand;

pub async fn handle_request(
    state: &Arc<Mutex<DaemonState>>,
    request: Request,
) -> Response {
    let st = state.lock().await;

    match request {
        Request::Status => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if let Err(e) = st.pw_handle.send(PwCommand::ListDevices { reply: tx }) {
                return Response::Error {
                    message: format!("Failed to send command: {e}"),
                };
            }
            drop(st);
            match rx.await {
                Ok(devices) => Response::Status {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    uptime_secs: state.lock().await.start_time.elapsed().as_secs_f64(),
                    devices,
                },
                Err(e) => Response::Error {
                    message: format!("Failed to get device list: {e}"),
                },
            }
        }

        Request::DevicesList => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if let Err(e) = st.pw_handle.send(PwCommand::ListDevices { reply: tx }) {
                return Response::Error {
                    message: format!("Failed to send command: {e}"),
                };
            }
            drop(st);
            match rx.await {
                Ok(devices) => Response::DevicesList { devices },
                Err(e) => Response::Error {
                    message: format!("Failed to get device list: {e}"),
                },
            }
        }

        Request::DevicesCreate {
            name,
            source_description,
            sink_description,
        } => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let source_desc = source_description.unwrap_or_else(|| format!("{name} Source"));
            let sink_desc = sink_description.unwrap_or_else(|| format!("{name} Sink"));
            if let Err(e) = st.pw_handle.send(PwCommand::CreateDevice {
                name: name.clone(),
                source_description: source_desc,
                sink_description: sink_desc,
                reply: tx,
            }) {
                return Response::Error {
                    message: format!("Failed to send command: {e}"),
                };
            }
            drop(st);
            match rx.await {
                Ok(Ok(())) => Response::DeviceCreated { name },
                Ok(Err(e)) => Response::Error { message: e },
                Err(e) => Response::Error {
                    message: format!("Channel error: {e}"),
                },
            }
        }

        Request::DevicesDestroy { name } => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if let Err(e) = st.pw_handle.send(PwCommand::DestroyDevice {
                name: name.clone(),
                reply: tx,
            }) {
                return Response::Error {
                    message: format!("Failed to send command: {e}"),
                };
            }
            drop(st);
            match rx.await {
                Ok(Ok(())) => Response::DeviceDestroyed { name },
                Ok(Err(e)) => Response::Error { message: e },
                Err(e) => Response::Error {
                    message: format!("Channel error: {e}"),
                },
            }
        }

        Request::Generate {
            device,
            waveform,
            frequency,
            volume,
            duration_secs,
            continuous: _,
        } => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if let Err(e) = st.pw_handle.send(PwCommand::StartTone {
                device: device.clone(),
                waveform,
                frequency,
                volume,
                duration_secs,
                reply: tx,
            }) {
                return Response::Error {
                    message: format!("Failed to send command: {e}"),
                };
            }
            drop(st);
            match rx.await {
                Ok(Ok(())) => Response::GenerateStarted { device },
                Ok(Err(e)) => Response::Error { message: e },
                Err(e) => Response::Error {
                    message: format!("Channel error: {e}"),
                },
            }
        }

        Request::Play {
            device,
            file_path,
            loop_count,
            volume,
        } => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if let Err(e) = st.pw_handle.send(PwCommand::PlayFile {
                device: device.clone(),
                file_path,
                loop_count,
                volume,
                reply: tx,
            }) {
                return Response::Error {
                    message: format!("Failed to send command: {e}"),
                };
            }
            drop(st);
            match rx.await {
                Ok(Ok(())) => Response::PlayStarted { device },
                Ok(Err(e)) => Response::Error { message: e },
                Err(e) => Response::Error {
                    message: format!("Channel error: {e}"),
                },
            }
        }

        Request::Record {
            device,
            file_path,
            format: _,
            duration_secs: _,
            sample_rate: _,
            channels: _,
        } => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if let Err(e) = st.pw_handle.send(PwCommand::StartRecord {
                device: device.clone(),
                file_path,
                reply: tx,
            }) {
                return Response::Error {
                    message: format!("Failed to send command: {e}"),
                };
            }
            drop(st);
            match rx.await {
                Ok(Ok(())) => Response::RecordStarted { device },
                Ok(Err(e)) => Response::Error { message: e },
                Err(e) => Response::Error {
                    message: format!("Channel error: {e}"),
                },
            }
        }

        Request::StreamStart {
            device,
            direction,
            sample_rate: _,
            channels: _,
            sample_format: _,
        } => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if let Err(e) = st.pw_handle.send(PwCommand::StartStream {
                device: device.clone(),
                direction,
                reply: tx,
            }) {
                return Response::Error {
                    message: format!("Failed to send command: {e}"),
                };
            }
            drop(st);
            match rx.await {
                Ok(Ok(ring_buffer)) => {
                    // Create a data socket for the CLI to connect to
                    let xdg = std::env::var("XDG_RUNTIME_DIR")
                        .unwrap_or_else(|_| "/tmp".to_string());
                    let data_socket_path = format!(
                        "{xdg}/audiomockd-stream-{device}-{}.sock",
                        std::process::id()
                    );
                    let _ = std::fs::remove_file(&data_socket_path);

                    let path_clone = data_socket_path.clone();
                    let dir = direction;

                    // Spawn a task to bridge the data socket and the ring buffer
                    tokio::spawn(async move {
                        if let Err(e) =
                            crate::ipc::stream_bridge::run(&path_clone, ring_buffer, dir).await
                        {
                            tracing::error!("Stream bridge error: {e}");
                        }
                        let _ = std::fs::remove_file(&path_clone);
                    });

                    // Give the listener a moment to bind
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                    Response::StreamStarted {
                        data_socket: data_socket_path,
                    }
                }
                Ok(Err(e)) => Response::Error { message: e },
                Err(e) => Response::Error {
                    message: format!("Channel error: {e}"),
                },
            }
        }

        Request::Stop { device } => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if let Err(e) = st.pw_handle.send(PwCommand::StopJob {
                device: device.clone(),
                reply: tx,
            }) {
                return Response::Error {
                    message: format!("Failed to send command: {e}"),
                };
            }
            drop(st);
            match rx.await {
                Ok(Ok(())) => Response::Stopped { device },
                Ok(Err(e)) => Response::Error { message: e },
                Err(e) => Response::Error {
                    message: format!("Channel error: {e}"),
                },
            }
        }
    }
}
