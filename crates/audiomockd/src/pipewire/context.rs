use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::Result;
use audiomock_proto::audio::{AudioFormat, SampleFormat};
use audiomock_proto::config::{AudioSection, DevicePairConfig};
use audiomock_proto::device::DeviceInfo;
use crossbeam_channel::Receiver;
use pipewire as pw;

use super::PwCommand;
use super::virtual_node::DevicePair;

pub fn run_pipewire_loop(
    cmd_rx: Receiver<PwCommand>,
    initial_devices: Vec<DevicePairConfig>,
    audio_config: AudioSection,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoop::new(None)?;
    let context = pw::context::Context::new(&mainloop)?;
    let core = context.connect(None)?;

    let devices: RefCell<HashMap<String, DevicePair>> = RefCell::new(HashMap::new());

    // Create initial device pairs from config
    for dev_config in &initial_devices {
        let source_desc = dev_config
            .source_description
            .clone()
            .unwrap_or_else(|| format!("{} Source", dev_config.name));
        let sink_desc = dev_config
            .sink_description
            .clone()
            .unwrap_or_else(|| format!("{} Sink", dev_config.name));

        let format = AudioFormat {
            sample_rate: audio_config.default_sample_rate,
            channels: audio_config.default_channels,
            sample_format: SampleFormat::F32LE,
        };

        match DevicePair::new(&core, &dev_config.name, &source_desc, &sink_desc, &format) {
            Ok(pair) => {
                tracing::info!("Created device pair: {}", dev_config.name);
                devices.borrow_mut().insert(dev_config.name.clone(), pair);
            }
            Err(e) => {
                tracing::error!("Failed to create device pair '{}': {e}", dev_config.name);
            }
        }
    }

    // Set up a timer to poll for commands from the IPC layer.
    // IMPORTANT: Each command must borrow and DROP the RefCell before the next
    // command, so that PipeWire process callbacks (which also borrow the state)
    // don't hit a double-borrow panic.
    let timer = mainloop.loop_().add_timer({
        let mainloop_weak = mainloop.downgrade();
        let core = core.clone();
        let audio_config = audio_config.clone();
        move |_expirations| {
            // Drain all pending commands, processing one at a time.
            // Each command borrows and releases devices within its own scope.
            while let Ok(cmd) = cmd_rx.try_recv() {
                process_command(cmd, &devices, &core, &audio_config, &mainloop_weak);
            }
        }
    });

    // Arm timer to fire every 10ms for command polling
    timer.update_timer(
        Some(std::time::Duration::from_millis(10)),
        Some(std::time::Duration::from_millis(10)),
    );

    tracing::info!("PipeWire main loop starting");
    mainloop.run();
    tracing::info!("PipeWire main loop exited");

    unsafe { pw::deinit() };
    Ok(())
}

/// Process a single command. Borrows are scoped tightly to avoid conflicts
/// with PipeWire process callbacks that run on the same thread.
fn process_command(
    cmd: PwCommand,
    devices: &RefCell<HashMap<String, DevicePair>>,
    core: &pw::core::Core,
    audio_config: &AudioSection,
    mainloop_weak: &pw::main_loop::WeakMainLoop,
) {
    match cmd {
        PwCommand::ListDevices { reply } => {
            let devs = devices.borrow();
            let infos: Vec<DeviceInfo> = devs.values().map(|d| d.info()).collect();
            let _ = reply.send(infos);
        }

        PwCommand::CreateDevice {
            name,
            source_description,
            sink_description,
            reply,
        } => {
            if devices.borrow().contains_key(&name) {
                let _ = reply.send(Err(format!("Device '{name}' already exists")));
                return;
            }
            let format = AudioFormat {
                sample_rate: audio_config.default_sample_rate,
                channels: audio_config.default_channels,
                sample_format: SampleFormat::F32LE,
            };
            match DevicePair::new(
                core,
                &name,
                &source_description,
                &sink_description,
                &format,
            ) {
                Ok(pair) => {
                    tracing::info!("Created device pair: {name}");
                    devices.borrow_mut().insert(name.clone(), pair);
                    let _ = reply.send(Ok(()));
                }
                Err(e) => {
                    let _ = reply.send(Err(format!("Failed to create device pair: {e}")));
                }
            }
        }

        PwCommand::DestroyDevice { name, reply } => {
            if devices.borrow_mut().remove(&name).is_some() {
                let _ = reply.send(Ok(()));
            } else {
                let _ = reply.send(Err(format!("Device '{name}' not found")));
            }
        }

        PwCommand::StartTone {
            device,
            waveform,
            frequency,
            volume,
            duration_secs,
            reply,
        } => {
            let mut devs = devices.borrow_mut();
            if let Some(dev) = devs.get_mut(&device) {
                dev.start_tone(waveform, frequency, volume, duration_secs);
                let _ = reply.send(Ok(()));
            } else {
                let _ = reply.send(Err(format!("Device '{device}' not found")));
            }
        }

        PwCommand::StopJob { device, reply } => {
            let mut devs = devices.borrow_mut();
            if let Some(dev) = devs.get_mut(&device) {
                dev.stop_job();
                let _ = reply.send(Ok(()));
            } else {
                let _ = reply.send(Err(format!("Device '{device}' not found")));
            }
        }

        PwCommand::PlayFile {
            device,
            file_path,
            loop_count,
            volume,
            reply,
        } => {
            let mut devs = devices.borrow_mut();
            if let Some(dev) = devs.get_mut(&device) {
                match dev.start_playback(&file_path, loop_count, volume) {
                    Ok(()) => { let _ = reply.send(Ok(())); }
                    Err(e) => { let _ = reply.send(Err(e.to_string())); }
                }
            } else {
                let _ = reply.send(Err(format!("Device '{device}' not found")));
            }
        }

        PwCommand::StartRecord {
            device,
            file_path,
            reply,
        } => {
            let mut devs = devices.borrow_mut();
            if let Some(dev) = devs.get_mut(&device) {
                match dev.start_recording(&file_path) {
                    Ok(()) => { let _ = reply.send(Ok(())); }
                    Err(e) => { let _ = reply.send(Err(e.to_string())); }
                }
            } else {
                let _ = reply.send(Err(format!("Device '{device}' not found")));
            }
        }

        PwCommand::StartStream {
            device,
            direction,
            reply,
        } => {
            let mut devs = devices.borrow_mut();
            if let Some(dev) = devs.get_mut(&device) {
                let rb = dev.start_stream(direction);
                let _ = reply.send(Ok(rb));
            } else {
                let _ = reply.send(Err(format!("Device '{device}' not found")));
            }
        }

        PwCommand::Shutdown => {
            if let Some(ml) = mainloop_weak.upgrade() {
                ml.quit();
            }
        }
    }
}
