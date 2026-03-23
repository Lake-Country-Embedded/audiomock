use anyhow::{Context, Result};
use audiomock_proto::audio::AudioFormat;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

/// Decode an audio file into interleaved f32 samples matching the target format.
pub fn decode_file(path: &str, target: &AudioFormat) -> Result<Vec<f32>> {
    gst::init().context("Failed to initialize GStreamer")?;

    let pipeline_str = format!(
        "filesrc location={path} ! decodebin ! audioconvert ! audioresample ! \
         audio/x-raw,format=F32LE,rate={rate},channels={ch},layout=interleaved ! \
         appsink name=sink",
        path = shell_escape(path),
        rate = target.sample_rate,
        ch = target.channels,
    );

    let pipeline = gst::parse::launch(&pipeline_str)
        .context("Failed to create GStreamer pipeline")?;

    let pipeline = pipeline
        .dynamic_cast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("Failed to cast to Pipeline"))?;

    let sink = pipeline
        .by_name("sink")
        .context("Failed to find appsink")?;
    let appsink = sink
        .dynamic_cast::<gst_app::AppSink>()
        .map_err(|_| anyhow::anyhow!("Failed to cast to AppSink"))?;

    pipeline
        .set_state(gst::State::Playing)
        .context("Failed to start pipeline")?;

    let mut samples = Vec::new();

    loop {
        let sample = match appsink.pull_sample() {
            Ok(s) => s,
            Err(_) => break, // EOS or error
        };

        let buffer = sample.buffer().context("No buffer in sample")?;
        let map = buffer
            .map_readable()
            .context("Failed to map buffer")?;

        let floats: &[f32] = bytemuck_cast_slice(map.as_slice());
        samples.extend_from_slice(floats);
    }

    pipeline
        .set_state(gst::State::Null)
        .context("Failed to stop pipeline")?;

    tracing::info!(
        "Decoded {path}: {} samples ({:.2}s at {}Hz, {}ch)",
        samples.len(),
        samples.len() as f64 / (target.sample_rate as f64 * target.channels as f64),
        target.sample_rate,
        target.channels,
    );

    Ok(samples)
}

fn bytemuck_cast_slice(bytes: &[u8]) -> &[f32] {
    let len = bytes.len() / 4;
    let ptr = bytes.as_ptr() as *const f32;
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

fn shell_escape(s: &str) -> String {
    // GStreamer pipeline parsing handles paths, but escape spaces
    s.replace(' ', "\\ ")
}
