//! Incremental recorder.
//!
//! The PipeWire process callback pushes samples into a lock-free ring buffer
//! and a writer thread drains it to disk. Memory stays bounded regardless of
//! recording length, and the real-time callback never touches the filesystem.
//!
//! Two sinks are available. When the output is WAV at the capture format we
//! write PCM directly and patch the header on finish. Anything that needs
//! re-encoding or rate/channel conversion goes through GStreamer instead.

use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use audiomock_proto::audio::{AudioFormat, OutputFormat};

use super::ring_buffer::RingBuffer;

/// Ring capacity in interleaved samples. At 48 kHz stereo this is ~2 seconds,
/// which is a generous cushion for a thread that only has to write to disk.
const RING_CAPACITY: usize = 48_000 * 2 * 2;

/// How many samples the writer thread moves per iteration.
const DRAIN_CHUNK: usize = 8192;

trait RecordSink: Send {
    fn write(&mut self, samples: &[f32]) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Direct WAV sink
// ---------------------------------------------------------------------------

struct WavSink {
    file: std::fs::File,
    data_bytes: u32,
    channels: u16,
    sample_rate: u32,
}

impl WavSink {
    fn new(path: &str, format: &AudioFormat) -> Result<Self> {
        let mut file = std::fs::File::create(path)
            .with_context(|| format!("Failed to create file: {path}"))?;
        // Placeholder header; sizes are patched in finish().
        write_wav_header(&mut file, format.channels, format.sample_rate, 0)?;
        Ok(Self {
            file,
            data_bytes: 0,
            channels: format.channels,
            sample_rate: format.sample_rate,
        })
    }
}

impl RecordSink for WavSink {
    fn write(&mut self, samples: &[f32]) -> Result<()> {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        self.file.write_all(&bytes)?;
        self.data_bytes = self.data_bytes.saturating_add(bytes.len() as u32);
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        self.file.seek(SeekFrom::Start(0))?;
        let (channels, rate, data) = (self.channels, self.sample_rate, self.data_bytes);
        write_wav_header(&mut self.file, channels, rate, data)?;
        self.file.flush()?;
        Ok(())
    }
}

fn write_wav_header(
    file: &mut std::fs::File,
    channels: u16,
    sample_rate: u32,
    data_bytes: u32,
) -> Result<()> {
    let bits_per_sample = 16u16;
    let byte_rate = sample_rate * channels as u32 * (bits_per_sample as u32 / 8);
    let block_align = channels * (bits_per_sample / 8);

    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_bytes).to_le_bytes())?;
    file.write_all(b"WAVE")?;
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&bits_per_sample.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_bytes.to_le_bytes())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// GStreamer sink (re-encoding and/or rate/channel conversion)
// ---------------------------------------------------------------------------

struct GstSink {
    pipeline: gstreamer::Pipeline,
    appsrc: gstreamer_app::AppSrc,
}

impl GstSink {
    fn new(
        path: &str,
        out_format: OutputFormat,
        src: &AudioFormat,
        target_rate: u32,
        target_channels: u16,
    ) -> Result<Self> {
        use gstreamer::prelude::*;

        gstreamer::init().context("Failed to initialize GStreamer")?;

        let encoder = match out_format {
            OutputFormat::Wav => "wavenc",
            OutputFormat::Flac => "flacenc",
            OutputFormat::Ogg => "vorbisenc ! oggmux",
        };

        let pipeline_str = format!(
            "appsrc name=src format=time do-timestamp=true is-live=true \
             caps=audio/x-raw,format=F32LE,rate={src_rate},channels={src_ch},layout=interleaved ! \
             audioconvert ! audioresample ! \
             audio/x-raw,rate={rate},channels={ch} ! \
             {encoder} ! filesink location={path}",
            src_rate = src.sample_rate,
            src_ch = src.channels,
            rate = target_rate,
            ch = target_channels,
        );

        let pipeline = gstreamer::parse::launch(&pipeline_str)
            .context("Failed to build encode pipeline")?
            .dynamic_cast::<gstreamer::Pipeline>()
            .map_err(|_| anyhow::anyhow!("Failed to cast to Pipeline"))?;

        let appsrc = pipeline
            .by_name("src")
            .context("appsrc not found")?
            .dynamic_cast::<gstreamer_app::AppSrc>()
            .map_err(|_| anyhow::anyhow!("Failed to cast to AppSrc"))?;

        pipeline
            .set_state(gstreamer::State::Playing)
            .context("Failed to start encode pipeline")?;

        Ok(Self { pipeline, appsrc })
    }
}

impl RecordSink for GstSink {
    fn write(&mut self, samples: &[f32]) -> Result<()> {
        let mut bytes = Vec::with_capacity(samples.len() * 4);
        for &s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let buffer = gstreamer::Buffer::from_mut_slice(bytes);
        self.appsrc
            .push_buffer(buffer)
            .map_err(|e| anyhow::anyhow!("appsrc push failed: {e:?}"))?;
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<()> {
        use gstreamer::prelude::*;
        self.appsrc
            .end_of_stream()
            .map_err(|e| anyhow::anyhow!("EOS failed: {e:?}"))?;

        if let Some(bus) = self.pipeline.bus() {
            for msg in bus.iter_timed(gstreamer::ClockTime::from_seconds(10)) {
                match msg.view() {
                    gstreamer::MessageView::Eos(_) => break,
                    gstreamer::MessageView::Error(e) => {
                        let _ = self.pipeline.set_state(gstreamer::State::Null);
                        anyhow::bail!("Encoding error: {}", e.error());
                    }
                    _ => {}
                }
            }
        }

        self.pipeline.set_state(gstreamer::State::Null)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Recorder
// ---------------------------------------------------------------------------

pub struct Recorder {
    rb: Arc<RingBuffer>,
    stop: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
    pub file_path: String,
}

impl Recorder {
    /// Start recording. `target_rate` and `target_channels` fall back to the
    /// capture format when the caller did not ask for anything specific.
    pub fn start(
        path: &str,
        out_format: OutputFormat,
        src_format: &AudioFormat,
        target_rate: Option<u32>,
        target_channels: Option<u16>,
    ) -> Result<Self> {
        let rate = target_rate.unwrap_or(src_format.sample_rate);
        let channels = target_channels.unwrap_or(src_format.channels);

        let needs_gst = out_format != OutputFormat::Wav
            || rate != src_format.sample_rate
            || channels != src_format.channels;

        let mut sink: Box<dyn RecordSink> = if needs_gst {
            Box::new(GstSink::new(path, out_format, src_format, rate, channels)?)
        } else {
            Box::new(WavSink::new(path, src_format)?)
        };

        let rb = RingBuffer::new(RING_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicU64::new(0));

        let handle = {
            let rb = rb.clone();
            let stop = stop.clone();
            let path = path.to_string();
            std::thread::Builder::new()
                .name("recorder".to_string())
                .spawn(move || {
                    let mut buf = vec![0.0f32; DRAIN_CHUNK];
                    loop {
                        let n = rb.read(&mut buf);
                        if n > 0 {
                            if let Err(e) = sink.write(&buf[..n]) {
                                tracing::error!("Recorder write failed: {e}");
                                break;
                            }
                        } else if stop.load(Ordering::Acquire) {
                            break;
                        } else {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                    }
                    if let Err(e) = sink.finish() {
                        tracing::error!("Failed to finalize recording {path}: {e}");
                    } else {
                        tracing::info!("Wrote recording to {path}");
                    }
                })?
        };

        Ok(Self {
            rb,
            stop,
            dropped,
            handle: Some(handle),
            file_path: path.to_string(),
        })
    }

    /// Push samples from the real-time callback. Lock-free and allocation-free.
    pub fn push(&self, samples: &[f32]) {
        let written = self.rb.write(samples);
        if written < samples.len() {
            self.dropped
                .fetch_add((samples.len() - written) as u64, Ordering::Relaxed);
        }
    }

    /// Stop the writer thread and finalize the file.
    pub fn finish(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let dropped = self.dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            tracing::warn!(
                "Recording {} dropped {dropped} samples (writer could not keep up)",
                self.file_path
            );
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Covers the path where a Recorder is dropped without finish(), so a
        // killed client still leaves a valid file behind.
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_format() -> AudioFormat {
        AudioFormat {
            sample_rate: 48_000,
            channels: 2,
            sample_format: audiomock_proto::audio::SampleFormat::F32LE,
        }
    }

    /// Read back the fields the WAV header must carry after finalization.
    fn parse_wav(path: &str) -> (u16, u32, u32) {
        let bytes = std::fs::read(path).expect("recording should exist");
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        let channels = u16::from_le_bytes(bytes[22..24].try_into().unwrap());
        let rate = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        let data = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
        // RIFF size must agree with the data chunk, or players read garbage.
        let riff = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(riff, 36 + data, "RIFF size not patched on finish");
        (channels, rate, data)
    }

    #[test]
    fn wav_header_is_patched_on_finish() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.wav").to_str().unwrap().to_string();
        let fmt = stereo_format();

        let rec = Recorder::start(&path, OutputFormat::Wav, &fmt, None, None).unwrap();
        // Exactly 0.5 s of stereo audio.
        let samples = vec![0.25f32; 48_000];
        rec.push(&samples);
        rec.finish();

        let (channels, rate, data) = parse_wav(&path);
        assert_eq!(channels, 2);
        assert_eq!(rate, 48_000);
        // 48000 interleaved samples at 16-bit = 96000 bytes.
        assert_eq!(data, 96_000, "unexpected payload size");
    }

    #[test]
    fn memory_stays_bounded_for_long_recordings() {
        // Push far more than the ring holds; the writer drains it to disk, so
        // nothing accumulates in RAM and the file still gets every sample.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.wav").to_str().unwrap().to_string();
        let fmt = stereo_format();

        let rec = Recorder::start(&path, OutputFormat::Wav, &fmt, None, None).unwrap();
        let chunk = vec![0.1f32; 4096];
        let iterations = (RING_CAPACITY / chunk.len()) * 3;
        for _ in 0..iterations {
            rec.push(&chunk);
            // Give the writer thread room to keep up.
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        rec.finish();

        let (_, _, data) = parse_wav(&path);
        let expected = (iterations * chunk.len() * 2) as u32;
        assert!(
            data > expected / 2,
            "writer lost most of the stream: {data} bytes of an expected {expected}"
        );
    }

    #[test]
    fn drop_without_finish_still_closes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dropped.wav").to_str().unwrap().to_string();
        let fmt = stereo_format();

        {
            let rec = Recorder::start(&path, OutputFormat::Wav, &fmt, None, None).unwrap();
            rec.push(&vec![0.5f32; 9600]);
            // No finish(); the Drop impl must still flush and patch.
        }

        let (_, _, data) = parse_wav(&path);
        assert_eq!(data, 19_200);
    }
}
