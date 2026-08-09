//! PipeWire graph linking, driven through the `pw-link` CLI.
//!
//! Lives in the daemon rather than the client so that `auto_link_pattern` can
//! reuse it the moment a device pair is created.

use anyhow::{Context, Result};

#[derive(Debug, Default, Clone, Copy)]
pub struct LinkReport {
    pub created: usize,
    pub removed: usize,
}

/// One directed link between two ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub output: String,
    pub input: String,
}

fn run_pw_link(args: &[&str]) -> Result<std::process::Output> {
    std::process::Command::new("pw-link")
        .args(args)
        .output()
        .context("Failed to run pw-link. Is PipeWire installed?")
}

fn list_ports(flag: &str) -> Result<Vec<String>> {
    let out = run_pw_link(&[flag])?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Parse `pw-link -l` into concrete links.
///
/// The listing indents each peer under its owning port, using `|->` for
/// "owner is the output" and `|<-` for "owner is the input".
pub fn list_links() -> Result<Vec<Link>> {
    let out = run_pw_link(&["-l"])?;
    Ok(parse_links(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse the `pw-link -l` listing into deduplicated directed links.
///
/// Every link shows up twice, once under each endpoint, so the result is
/// sorted and deduplicated before being returned.
fn parse_links(text: &str) -> Vec<Link> {
    let mut links = Vec::new();
    let mut owner: Option<String> = None;

    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let trimmed = line.trim();
        if !line.starts_with(char::is_whitespace) {
            owner = Some(trimmed.to_string());
            continue;
        }
        let Some(ref own) = owner else { continue };
        if let Some(peer) = trimmed.strip_prefix("|-> ") {
            links.push(Link {
                output: own.clone(),
                input: peer.trim().to_string(),
            });
        } else if let Some(peer) = trimmed.strip_prefix("|<- ") {
            links.push(Link {
                output: peer.trim().to_string(),
                input: own.clone(),
            });
        }
    }

    links.sort_by(|a, b| (&a.output, &a.input).cmp(&(&b.output, &b.input)));
    links.dedup();
    links
}

fn source_node(device: &str) -> String {
    format!("audiomock-source-{device}:")
}

fn sink_node(device: &str) -> String {
    format!("audiomock-sink-{device}:")
}

fn touches_device(link: &Link, device: &str) -> bool {
    let (src, snk) = (source_node(device), sink_node(device));
    [&link.output, &link.input]
        .iter()
        .any(|p| p.starts_with(&src) || p.starts_with(&snk))
}

/// Remove links between the given peer ports and anything that is not ours.
///
/// [`unlink_device`] only drops links that touch *our* nodes, so a session
/// manager's auto-routing on the peer survives it: the host microphone keeps
/// feeding the guest's mic input, and the guest's speaker output keeps reaching
/// real hardware. On the capture side that is merely noisy, because PipeWire
/// fans the guest's output out to both destinations. On the **injection** side
/// it is a correctness problem -- the host mic mixes into the guest's mic
/// alongside whatever the test is injecting, so a test measuring an injected
/// tone is really measuring that tone plus the room.
///
/// Links to our own pair are left alone so this is safe to call after linking.
fn unlink_foreign(ports: &[&String], device: &str) -> Result<usize> {
    let (src, snk) = (source_node(device), sink_node(device));
    let mut removed = 0usize;
    for link in list_links()? {
        let touches_peer = ports
            .iter()
            .any(|p| ***p == link.output || ***p == link.input);
        if !touches_peer {
            continue;
        }
        let is_ours = [&link.output, &link.input]
            .iter()
            .any(|p| p.starts_with(&src) || p.starts_with(&snk));
        if is_ours {
            continue;
        }
        let status = run_pw_link(&["-d", &link.output, &link.input])?;
        if status.status.success() {
            tracing::info!(
                "Exclusive: dropped foreign link {} -> {}",
                link.output,
                link.input
            );
            removed += 1;
        } else {
            tracing::warn!(
                "Exclusive: failed to drop {} -> {}: {}",
                link.output,
                link.input,
                String::from_utf8_lossy(&status.stderr).trim()
            );
        }
    }
    Ok(removed)
}

/// Remove every link touching this device pair, whoever created it.
pub fn unlink_device(device: &str) -> Result<LinkReport> {
    let mut report = LinkReport::default();
    for link in list_links()?.iter().filter(|l| touches_device(l, device)) {
        let status = run_pw_link(&["-d", &link.output, &link.input])?;
        if status.status.success() {
            report.removed += 1;
        } else {
            tracing::warn!(
                "Failed to unlink {} -> {}: {}",
                link.output,
                link.input,
                String::from_utf8_lossy(&status.stderr).trim()
            );
        }
    }
    Ok(report)
}

/// Link a device pair to nodes whose port names match `pattern`.
///
/// Guest speaker output lands on our sink; our source feeds the guest mic.
///
/// When `exclusive` is set this drops pre-existing links on **both** sides:
/// ours, so re-linking is idempotent, and the matched peer's, so a session
/// manager's auto-routing does not survive alongside us. Only clearing our own
/// side is not enough -- the auto-routing links never touch our ports, so they
/// used to be left in place, leaving the host microphone wired into the guest's
/// mic input next to whatever a test was injecting.
pub fn link_device(device: &str, pattern: &str, exclusive: bool) -> Result<LinkReport> {
    let mut report = if exclusive {
        unlink_device(device)?
    } else {
        LinkReport::default()
    };

    let needle = pattern.to_lowercase();
    let outputs = list_ports("-o")?;
    let inputs = list_ports("-i")?;

    let matches = |l: &&String| {
        l.to_lowercase().contains(&needle) && !l.starts_with("audiomock-")
    };

    // Guest speaker -> our sink. Monitor ports are for observation, not routing.
    let peer_outputs: Vec<&String> = outputs
        .iter()
        .filter(|l| matches(l) && !l.contains("monitor_"))
        .collect();
    let our_sink_inputs: Vec<&String> =
        inputs.iter().filter(|l| l.starts_with(&sink_node(device))).collect();

    // Our source -> guest mic.
    let peer_inputs: Vec<&String> = inputs.iter().filter(matches).collect();
    let our_source_outputs: Vec<&String> = outputs
        .iter()
        .filter(|l| l.starts_with(&source_node(device)))
        .collect();

    if peer_outputs.is_empty() && peer_inputs.is_empty() {
        anyhow::bail!("No PipeWire nodes matched pattern '{pattern}'");
    }

    // Exclusivity has to cover the *peer's* existing routing too, not just
    // ours. Done after the peer ports are known and before our own links are
    // made, so nothing we just created is torn down again.
    if exclusive {
        let mut peers: Vec<&String> = Vec::new();
        peers.extend(peer_outputs.iter().copied());
        peers.extend(peer_inputs.iter().copied());
        report.removed += unlink_foreign(&peers, device)?;
    }

    if our_sink_inputs.len() != peer_outputs.len() && !peer_outputs.is_empty() {
        tracing::info!(
            "Channel count differs on capture side: {} peer port(s) vs {} sink port(s); \
             linking the first {}",
            peer_outputs.len(),
            our_sink_inputs.len(),
            peer_outputs.len().min(our_sink_inputs.len())
        );
    }
    if our_source_outputs.len() != peer_inputs.len() && !peer_inputs.is_empty() {
        tracing::info!(
            "Channel count differs on playback side: {} source port(s) vs {} peer port(s); \
             linking the first {}",
            our_source_outputs.len(),
            peer_inputs.len(),
            our_source_outputs.len().min(peer_inputs.len())
        );
    }

    for (peer, ours) in peer_outputs.iter().zip(our_sink_inputs.iter()) {
        if connect(peer, ours)? {
            report.created += 1;
        }
    }
    for (ours, peer) in our_source_outputs.iter().zip(peer_inputs.iter()) {
        if connect(ours, peer)? {
            report.created += 1;
        }
    }

    Ok(report)
}

fn connect(output: &str, input: &str) -> Result<bool> {
    let out = run_pw_link(&[output, input])?;
    if out.status.success() {
        tracing::info!("Linked {output} -> {input}");
        Ok(true)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        // An already-present link is not a failure worth propagating.
        if err.contains("File exists") {
            Ok(false)
        } else {
            tracing::warn!("Failed to link {output} -> {input}: {}", err.trim());
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_notations_collapse_to_one_directed_link() {
        // Real `pw-link -l` shape: owner line, then indented peers. The same
        // link appears under both endpoints, as |-> and as |<-.
        let sample = "\
nodeA:out_FL
  |-> nodeB:in_FL
nodeB:in_FL
  |<- nodeA:out_FL
";
        let links = parse_links(sample);
        assert_eq!(links.len(), 1, "duplicate listing should collapse");
        assert_eq!(links[0].output, "nodeA:out_FL");
        assert_eq!(links[0].input, "nodeB:in_FL");
    }

    #[test]
    fn parses_multiple_peers_under_one_owner() {
        // A port fanned out to two destinations, which is what a QEMU speaker
        // linked to both real hardware and a capture sink looks like.
        let sample = "\
qemu:output_FL
  |-> speakers:playback_FL
  |-> audiomock-sink-mps:input_FL
";
        let links = parse_links(sample);
        assert_eq!(links.len(), 2);
        assert!(links.iter().all(|l| l.output == "qemu:output_FL"));
        assert_eq!(
            links.iter().filter(|l| touches_device(l, "mps")).count(),
            1,
            "only the audiomock link belongs to device 'mps'"
        );
    }

    #[test]
    fn device_ownership_matches_either_end() {
        let l = Link {
            output: "qemu-mps:output_FL".to_string(),
            input: "audiomock-sink-mps:input_FL".to_string(),
        };
        assert!(touches_device(&l, "mps"));
        assert!(!touches_device(&l, "other"));
    }
}
