use anyhow::{Context, Result};

/// Link QEMU PipeWire nodes to our virtual device pair using pw-link.
pub async fn run(device: &str, qemu_node: Option<&str>) -> Result<()> {
    // Discover QEMU nodes by listing PipeWire ports
    let output = tokio::process::Command::new("pw-link")
        .arg("-o")
        .output()
        .await
        .context("Failed to run pw-link. Is PipeWire installed?")?;

    let output_ports = String::from_utf8_lossy(&output.stdout);

    let output2 = tokio::process::Command::new("pw-link")
        .arg("-i")
        .output()
        .await?;

    let input_ports = String::from_utf8_lossy(&output2.stdout);

    let qemu_pattern = qemu_node.unwrap_or("qemu");

    let pattern_lower = qemu_pattern.to_lowercase();

    // Find QEMU output ports (guest speaker -> our sink)
    // Exclude monitor ports — those are for observing, not routing
    let qemu_outputs: Vec<&str> = output_ports
        .lines()
        .filter(|l| l.to_lowercase().contains(&pattern_lower)
                && !l.contains("monitor_")
                && !l.contains(&format!("audiomock-")))
        .collect();

    // Find QEMU input ports (our source -> guest mic)
    let qemu_inputs: Vec<&str> = input_ports
        .lines()
        .filter(|l| l.to_lowercase().contains(&pattern_lower)
                && !l.contains(&format!("audiomock-")))
        .collect();

    // Find our sink input ports
    let sink_name = format!("audiomock-sink-{device}");
    let our_sink_inputs: Vec<&str> = input_ports
        .lines()
        .filter(|l| l.contains(&sink_name))
        .collect();

    // Find our source output ports
    let source_name = format!("audiomock-source-{device}");
    let our_source_outputs: Vec<&str> = output_ports
        .lines()
        .filter(|l| l.contains(&source_name))
        .collect();

    if qemu_outputs.is_empty() && qemu_inputs.is_empty() {
        eprintln!("No QEMU nodes found matching pattern '{qemu_pattern}'.");
        eprintln!("Available output ports:");
        for line in output_ports.lines().take(20) {
            eprintln!("  {line}");
        }
        eprintln!("Available input ports:");
        for line in input_ports.lines().take(20) {
            eprintln!("  {line}");
        }
        std::process::exit(1);
    }

    let mut linked = 0;

    // Link QEMU outputs -> our sink inputs (capture guest speaker)
    for (qemu_port, our_port) in qemu_outputs.iter().zip(our_sink_inputs.iter()) {
        let qemu_port = qemu_port.trim();
        let our_port = our_port.trim();
        println!("Linking {qemu_port} -> {our_port}");
        let status = tokio::process::Command::new("pw-link")
            .arg(qemu_port)
            .arg(our_port)
            .status()
            .await?;
        if status.success() {
            linked += 1;
        } else {
            eprintln!("  Failed to link (may already be linked)");
        }
    }

    // Link our source outputs -> QEMU inputs (feed guest mic)
    for (our_port, qemu_port) in our_source_outputs.iter().zip(qemu_inputs.iter()) {
        let our_port = our_port.trim();
        let qemu_port = qemu_port.trim();
        println!("Linking {our_port} -> {qemu_port}");
        let status = tokio::process::Command::new("pw-link")
            .arg(our_port)
            .arg(qemu_port)
            .status()
            .await?;
        if status.success() {
            linked += 1;
        } else {
            eprintln!("  Failed to link (may already be linked)");
        }
    }

    println!("Created {linked} links.");
    Ok(())
}
