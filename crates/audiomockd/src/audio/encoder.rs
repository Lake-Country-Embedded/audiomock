use anyhow::{Context, Result};
use audiomock_proto::audio::AudioFormat;

/// Write interleaved f32 samples to a WAV file.
pub fn write_wav(samples: &[f32], path: &str, format: &AudioFormat) -> Result<()> {
    use std::io::Write;

    let num_channels = format.channels as u32;
    let sample_rate = format.sample_rate;
    let bits_per_sample = 16u16; // Convert f32 -> i16 for WAV
    let byte_rate = sample_rate * num_channels * (bits_per_sample as u32 / 8);
    let block_align = num_channels as u16 * (bits_per_sample / 8);

    // Convert f32 samples to i16
    let i16_samples: Vec<i16> = samples
        .iter()
        .map(|&s| {
            let clamped = s.clamp(-1.0, 1.0);
            (clamped * 32767.0) as i16
        })
        .collect();

    let data_size = (i16_samples.len() * 2) as u32;
    let file_size = 36 + data_size;

    let mut file = std::fs::File::create(path)
        .with_context(|| format!("Failed to create file: {path}"))?;

    // RIFF header
    file.write_all(b"RIFF")?;
    file.write_all(&file_size.to_le_bytes())?;
    file.write_all(b"WAVE")?;

    // fmt chunk
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?; // chunk size
    file.write_all(&1u16.to_le_bytes())?; // PCM format
    file.write_all(&(num_channels as u16).to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&bits_per_sample.to_le_bytes())?;

    // data chunk
    file.write_all(b"data")?;
    file.write_all(&data_size.to_le_bytes())?;

    for sample in &i16_samples {
        file.write_all(&sample.to_le_bytes())?;
    }

    tracing::info!(
        "Wrote recording to {path}: {} samples ({:.2}s)",
        samples.len(),
        samples.len() as f64 / (sample_rate as f64 * num_channels as f64),
    );

    Ok(())
}

/// Write samples using GStreamer for non-WAV formats (FLAC, OGG).
pub fn write_encoded(
    samples: &[f32],
    path: &str,
    format: &AudioFormat,
    output_format: &str,
) -> Result<()> {
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app as gst_app;

    gst::init().context("Failed to initialize GStreamer")?;

    let encoder = match output_format {
        "flac" => "flacenc ! flacparse",
        "ogg" => "audioconvert ! vorbisenc ! oggmux",
        _ => return write_wav(samples, path, format),
    };

    let pipeline_str = format!(
        "appsrc name=src format=time caps=audio/x-raw,format=F32LE,rate={rate},channels={ch},layout=interleaved ! \
         {encoder} ! filesink location={path}",
        rate = format.sample_rate,
        ch = format.channels,
        encoder = encoder,
        path = path,
    );

    let pipeline = gst::parse::launch(&pipeline_str)
        .context("Failed to create GStreamer encode pipeline")?;
    let pipeline = pipeline
        .dynamic_cast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("Failed to cast to Pipeline"))?;

    let src = pipeline
        .by_name("src")
        .context("Failed to find appsrc")?;
    let appsrc = src
        .dynamic_cast::<gst_app::AppSrc>()
        .map_err(|_| anyhow::anyhow!("Failed to cast to AppSrc"))?;

    pipeline
        .set_state(gst::State::Playing)
        .context("Failed to start encode pipeline")?;

    // Push samples as bytes
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(
            samples.as_ptr() as *const u8,
            samples.len() * 4,
        )
    };
    let buffer = gst::Buffer::from_slice(bytes.to_vec());
    appsrc.push_buffer(buffer).context("Failed to push buffer")?;
    appsrc.end_of_stream().context("Failed to send EOS")?;

    // Wait for EOS
    let bus = pipeline.bus().context("No bus")?;
    for msg in bus.iter_timed(gst::ClockTime::NONE) {
        match msg.view() {
            gst::MessageView::Eos(_) => break,
            gst::MessageView::Error(e) => {
                pipeline.set_state(gst::State::Null)?;
                anyhow::bail!("Encoding error: {}", e.error());
            }
            _ => {}
        }
    }

    pipeline.set_state(gst::State::Null)?;
    Ok(())
}
