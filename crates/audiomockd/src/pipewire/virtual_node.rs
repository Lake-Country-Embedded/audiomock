use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use audiomock_proto::audio::{AudioFormat, OutputFormat, SampleFormat, WaveformKind};
use audiomock_proto::device::{ActiveJob, DeviceInfo, StreamDirection};
use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;
use pw::stream::{Stream, StreamFlags, StreamRef};

use super::format;
use crate::audio::recorder::Recorder;
use crate::audio::ring_buffer::RingBuffer;

/// Loopback ring size in interleaved samples: ~200 ms at 48 kHz stereo, small
/// enough that a guest hearing itself is not listening to stale audio.
const LOOPBACK_CAPACITY: usize = 19_200;

/// Silence pre-loaded into the loopback ring, ~50 ms at 48 kHz stereo.
///
/// The source and sink callbacks both run once per graph cycle, and the source
/// may well run first. Starting from an empty ring it would then read zeros
/// every cycle and only pick up stragglers, which passes audio through at a
/// level tens of dB below the original. The cushion keeps a whole quantum in
/// hand so each read is satisfied in full.
const LOOPBACK_PRIME: usize = 4_800;

/// Convert a normalized f32 sample into `fmt` and write it at `dst`.
#[inline]
fn write_sample(dst: &mut [u8], v: f32, fmt: SampleFormat) {
    match fmt {
        SampleFormat::F32LE => dst.copy_from_slice(&v.to_le_bytes()),
        SampleFormat::S16LE => {
            let s = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
            dst.copy_from_slice(&s.to_le_bytes());
        }
        SampleFormat::S32LE => {
            let s = (v.clamp(-1.0, 1.0) * 2_147_483_647.0) as i32;
            dst.copy_from_slice(&s.to_le_bytes());
        }
    }
}

/// Read one sample of `fmt` from `src` and normalize it to f32.
#[inline]
fn read_sample(src: &[u8], fmt: SampleFormat) -> f32 {
    match fmt {
        SampleFormat::F32LE => f32::from_le_bytes(src.try_into().unwrap_or([0; 4])),
        SampleFormat::S16LE => {
            i16::from_le_bytes(src.try_into().unwrap_or([0; 2])) as f32 / 32768.0
        }
        SampleFormat::S32LE => {
            i32::from_le_bytes(src.try_into().unwrap_or([0; 4])) as f32 / 2_147_483_648.0
        }
    }
}

/// Shared state for the virtual source, accessible from both commands and callbacks.
pub struct SourceState {
    pub format: AudioFormat,
    pub active_job: Option<ActiveJob>,
    pub tone_phase: f64,
    pub tone_frequency: f64,
    pub tone_volume: f32,
    pub tone_waveform: WaveformKind,
    pub tone_samples_produced: u64,
    pub tone_duration_samples: Option<u64>,
    pub playback_buffer: Vec<f32>,
    pub playback_pos: usize,
    pub playback_loop_count: Option<u32>,
    pub playback_loops_done: u32,
    pub playback_volume: f32,
    /// Ring buffer for stream-in and loopback: written elsewhere, read by the
    /// process callback.
    pub stream_in_buffer: Option<Arc<RingBuffer>>,
}

/// Shared state for the virtual sink.
pub struct SinkState {
    pub format: AudioFormat,
    pub active_job: Option<ActiveJob>,
    pub recorder: Option<Recorder>,
    /// Interleaved-sample budget for a duration-limited recording.
    pub record_limit_samples: Option<u64>,
    pub record_samples: u64,
    /// Wall-clock stop time. Sample counting alone would never expire on a
    /// sink that no peer is driving, so a silent capture would hang forever.
    pub record_deadline: Option<std::time::Instant>,
    /// Set by the callback when the budget runs out; finalized off the
    /// real-time thread by `DevicePair::poll`.
    pub record_complete: bool,
    /// Ring buffer for stream-out: process callback writes, IPC reads.
    pub stream_out_buffer: Option<Arc<RingBuffer>>,
    /// Ring buffer for loopback: process callback writes, source reads.
    pub loopback_buffer: Option<Arc<RingBuffer>>,
}

pub struct DevicePair {
    pub name: String,
    pub source_description: String,
    pub sink_description: String,
    pub source_state: Rc<RefCell<SourceState>>,
    pub sink_state: Rc<RefCell<SinkState>>,
    _source_stream: Stream,
    _sink_stream: Stream,
    // StreamListener is parameterized with () since we use Rc<RefCell> for state
    _source_listener: pw::stream::StreamListener<()>,
    _sink_listener: pw::stream::StreamListener<()>,
}

impl DevicePair {
    pub fn new(
        core: &pw::core::Core,
        name: &str,
        source_desc: &str,
        sink_desc: &str,
        audio_format: &AudioFormat,
        autoconnect: bool,
        buffer_size: u32,
    ) -> Result<Self> {
        let source_state = Rc::new(RefCell::new(SourceState {
            format: *audio_format,
            active_job: None,
            tone_phase: 0.0,
            tone_frequency: 440.0,
            tone_volume: 1.0,
            tone_waveform: WaveformKind::Sine,
            tone_samples_produced: 0,
            tone_duration_samples: None,
            playback_buffer: Vec::new(),
            playback_pos: 0,
            playback_loop_count: None,
            playback_loops_done: 0,
            playback_volume: 1.0,
            stream_in_buffer: None,
        }));

        let sink_state = Rc::new(RefCell::new(SinkState {
            format: *audio_format,
            active_job: None,
            recorder: None,
            record_limit_samples: None,
            record_samples: 0,
            record_deadline: None,
            record_complete: false,
            stream_out_buffer: None,
            loopback_buffer: None,
        }));

        // Requested quantum, honoring the configured buffer size.
        let latency = format!("{}/{}", buffer_size.max(1), audio_format.sample_rate);

        // Without AUTOCONNECT the session manager leaves these streams alone,
        // so the graph only ever holds links we asked for.
        let mut flags = StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS;
        if autoconnect {
            flags |= StreamFlags::AUTOCONNECT;
        }

        // --- Source stream (virtual mic, produces audio) ---
        // Use Stream/Output/Audio so PipeWire exposes output ports that
        // can be linked to QEMU's input (mic). Audio/Source/Virtual creates
        // a device node whose ports aren't directly linkable.
        let source_props = pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_CLASS => "Stream/Output/Audio",
            *pw::keys::NODE_NAME => format!("audiomock-source-{name}"),
            *pw::keys::NODE_LATENCY => latency.clone(),
            "node.always-process" => "true",
            "node.want-driver" => "true",
            *pw::keys::NODE_DESCRIPTION => source_desc,
        };

        let source_stream =
            Stream::new(core, &format!("audiomock-source-{name}"), source_props)?;

        let src_state_ref = source_state.clone();

        let source_listener = source_stream
            .add_local_listener_with_user_data(())
            .process(move |stream, _| {
                source_process(stream, &src_state_ref);
            })
            .register()?;

        let format_bytes = format::build_audio_format_bytes(audio_format);
        let mut params = [Pod::from_bytes(&format_bytes).unwrap()];

        source_stream.connect(
            spa::utils::Direction::Output,
            None,
            flags,
            &mut params,
        )?;

        // --- Sink stream (virtual speaker, consumes audio) ---
        // Use Stream/Input/Audio so PipeWire exposes input ports that
        // can receive audio from QEMU's output (speaker).
        let sink_props = pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_CLASS => "Stream/Input/Audio",
            *pw::keys::NODE_NAME => format!("audiomock-sink-{name}"),
            *pw::keys::NODE_LATENCY => latency,
            "node.always-process" => "true",
            "node.want-driver" => "true",
            *pw::keys::NODE_DESCRIPTION => sink_desc,
        };

        let sink_stream = Stream::new(core, &format!("audiomock-sink-{name}"), sink_props)?;

        let snk_state_ref = sink_state.clone();

        let sink_listener = sink_stream
            .add_local_listener_with_user_data(())
            .process(move |stream, _| {
                sink_process(stream, &snk_state_ref);
            })
            .register()?;

        let sink_format_bytes = format::build_audio_format_bytes(audio_format);
        let mut sink_params = [Pod::from_bytes(&sink_format_bytes).unwrap()];

        sink_stream.connect(
            spa::utils::Direction::Input,
            None,
            flags,
            &mut sink_params,
        )?;

        Ok(Self {
            name: name.to_string(),
            source_description: source_desc.to_string(),
            sink_description: sink_desc.to_string(),
            source_state,
            sink_state,
            _source_stream: source_stream,
            _sink_stream: sink_stream,
            _source_listener: source_listener,
            _sink_listener: sink_listener,
        })
    }

    pub fn info(&self) -> DeviceInfo {
        let src = self.source_state.borrow();
        let snk = self.sink_state.borrow();
        // Show source active job, or sink active job
        let active_job = src.active_job.clone().or_else(|| snk.active_job.clone());
        DeviceInfo {
            name: self.name.clone(),
            source_description: self.source_description.clone(),
            sink_description: self.sink_description.clone(),
            format: src.format,
            active_job,
        }
    }

    /// Finalize work the real-time callback flagged but must not do itself.
    /// Called from the daemon's command timer.
    pub fn poll(&mut self) {
        if tracing::enabled!(tracing::Level::DEBUG)
            && let Ok(st) = self.sink_state.try_borrow()
            && let Some(rb) = st.loopback_buffer.as_ref()
        {
            tracing::debug!(
                "loopback ring on '{}': {} samples queued",
                self.name,
                rb.available()
            );
        }
        let finished = {
            let mut st = match self.sink_state.try_borrow_mut() {
                Ok(s) => s,
                Err(_) => return,
            };
            let expired = st
                .record_deadline
                .is_some_and(|d| std::time::Instant::now() >= d);
            if !st.record_complete && !expired {
                return;
            }
            st.record_complete = false;
            st.active_job = None;
            st.record_limit_samples = None;
            st.record_deadline = None;
            st.record_samples = 0;
            st.recorder.take()
        };
        if let Some(rec) = finished {
            tracing::info!("Recording duration reached on '{}'", self.name);
            rec.finish();
        }
    }

    pub fn start_tone(
        &mut self,
        waveform: WaveformKind,
        frequency: f64,
        volume: f32,
        duration_secs: Option<f64>,
    ) {
        let mut state = self.source_state.borrow_mut();
        let duration_samples =
            duration_secs.map(|d| (d * state.format.sample_rate as f64) as u64);
        state.active_job = Some(ActiveJob::Tone {
            waveform,
            frequency,
            volume,
        });
        state.tone_waveform = waveform;
        state.tone_frequency = frequency;
        state.tone_volume = volume;
        state.tone_phase = 0.0;
        state.tone_samples_produced = 0;
        state.tone_duration_samples = duration_samples;
        tracing::info!(
            "Starting {waveform} tone at {frequency}Hz vol={volume} on '{}'",
            self.name
        );
    }

    pub fn stop_job(&mut self) {
        {
            let mut state = self.source_state.borrow_mut();
            state.active_job = None;
            state.playback_buffer.clear();
            state.playback_pos = 0;
            state.stream_in_buffer = None;
        }
        let recorder = {
            let mut state = self.sink_state.borrow_mut();
            state.active_job = None;
            state.stream_out_buffer = None;
            state.loopback_buffer = None;
            state.record_limit_samples = None;
            state.record_deadline = None;
            state.record_samples = 0;
            state.record_complete = false;
            state.recorder.take()
        };
        if let Some(rec) = recorder {
            rec.finish();
        }
        tracing::info!("Stopped job on '{}'", self.name);
    }

    pub fn start_playback(
        &mut self,
        file_path: &str,
        loop_count: Option<u32>,
        volume: f32,
    ) -> Result<()> {
        let format = self.source_state.borrow().format;
        let samples = crate::audio::decoder::decode_file(file_path, &format)?;

        let mut state = self.source_state.borrow_mut();
        state.playback_buffer = samples;
        state.playback_pos = 0;
        state.playback_loop_count = loop_count;
        state.playback_loops_done = 0;
        state.playback_volume = volume;
        state.active_job = Some(ActiveJob::Playback {
            file_path: file_path.to_string(),
            looping: loop_count.is_some_and(|n| n != 1),
            volume,
        });
        tracing::info!("Starting playback of '{file_path}' on '{}'", self.name);
        Ok(())
    }

    /// Start streaming. Returns the ring buffer that the IPC layer should use.
    pub fn start_stream(&mut self, direction: StreamDirection) -> Arc<RingBuffer> {
        // 1 second of stereo F32LE at 48kHz = 48000 * 2 = 96000 samples
        let rb = RingBuffer::new(96000);
        match direction {
            StreamDirection::In => {
                let mut state = self.source_state.borrow_mut();
                state.stream_in_buffer = Some(rb.clone());
                state.active_job = Some(ActiveJob::Streaming {
                    direction: StreamDirection::In,
                });
                tracing::info!("Started stream-in on '{}'", self.name);
            }
            StreamDirection::Out => {
                let mut state = self.sink_state.borrow_mut();
                state.stream_out_buffer = Some(rb.clone());
                state.active_job = Some(ActiveJob::Streaming {
                    direction: StreamDirection::Out,
                });
                tracing::info!("Started stream-out on '{}'", self.name);
            }
        }
        rb
    }

    /// Route this pair's sink back into its own source.
    ///
    /// The audio makes the trip inside the daemon, so the two peer nodes are
    /// never linked to each other and no scheduling cycle can form in the
    /// PipeWire graph.
    pub fn start_loopback(&mut self) {
        let rb = RingBuffer::new(LOOPBACK_CAPACITY);
        rb.write(&vec![0.0f32; LOOPBACK_PRIME]);
        {
            let mut sink = self.sink_state.borrow_mut();
            sink.loopback_buffer = Some(rb.clone());
            sink.active_job = Some(ActiveJob::Loopback);
        }
        {
            let mut src = self.source_state.borrow_mut();
            src.stream_in_buffer = Some(rb);
            src.active_job = Some(ActiveJob::Loopback);
        }
        tracing::info!("Started loopback on '{}'", self.name);
    }

    pub fn start_recording(
        &mut self,
        file_path: &str,
        out_format: OutputFormat,
        duration_secs: Option<f64>,
        sample_rate: Option<u32>,
        channels: Option<u16>,
    ) -> Result<()> {
        let format = self.sink_state.borrow().format;
        let recorder = Recorder::start(
            file_path,
            out_format,
            &format,
            sample_rate,
            channels,
        )?;

        let mut state = self.sink_state.borrow_mut();
        state.record_limit_samples = duration_secs.map(|d| {
            (d * format.sample_rate as f64) as u64 * format.channels as u64
        });
        state.record_deadline = duration_secs.map(|d| {
            // Small grace period so the sample budget, which is exact, wins
            // whenever audio is actually flowing.
            std::time::Instant::now()
                + std::time::Duration::from_secs_f64(d + 0.5)
        });
        state.record_samples = 0;
        state.record_complete = false;
        state.recorder = Some(recorder);
        state.active_job = Some(ActiveJob::Recording {
            file_path: file_path.to_string(),
        });
        tracing::info!(
            "Starting {out_format} recording to '{file_path}' on '{}'",
            self.name
        );
        Ok(())
    }
}

fn source_process(stream: &StreamRef, state: &Rc<RefCell<SourceState>>) {
    let mut buffer = match stream.dequeue_buffer() {
        Some(b) => b,
        None => return,
    };

    // How many frames the graph actually wants this cycle. The mapped buffer
    // is far larger than one quantum, so filling all of it makes the node
    // produce long bursts at long intervals instead of a steady stream. That
    // is inaudible for a continuous tone but wrecks anything rate-coupled,
    // such as draining a loopback ring.
    let requested = buffer.requested() as usize;

    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return;
    }

    let data = &mut datas[0];

    // Read the format before borrowing the buffer slice mutably.
    let (channels, sample_fmt) = {
        match state.try_borrow() {
            Ok(st) => (st.format.channels as usize, st.format.sample_format),
            Err(_) => return,
        }
    };
    let bps = sample_fmt.bytes_per_sample();
    let stride = bps * channels;

    let n_frames = if let Some(slice) = data.data() {
        let capacity_frames = slice.len() / stride;
        let n_frames = if requested > 0 {
            requested.min(capacity_frames)
        } else {
            capacity_frames
        };
        // Use try_borrow_mut to avoid panicking if a command handler
        // currently holds the borrow (timer + process callback can overlap).
        let mut st = match state.try_borrow_mut() {
            Ok(s) => s,
            Err(_) => {
                slice.fill(0); // Output silence if we can't borrow
                return;
            }
        };

        match &st.active_job {
            Some(ActiveJob::Tone { .. }) => {
                let sample_rate = st.format.sample_rate;
                let phase_inc = st.tone_frequency / sample_rate as f64;
                let waveform = st.tone_waveform;
                let volume = st.tone_volume;

                for i in 0..n_frames {
                    let sample = match waveform {
                        WaveformKind::Sine => (st.tone_phase * 2.0 * PI).sin(),
                        WaveformKind::Square => {
                            if (st.tone_phase * 2.0 * PI).sin() >= 0.0 {
                                1.0
                            } else {
                                -1.0
                            }
                        }
                        WaveformKind::Sawtooth => {
                            2.0 * (st.tone_phase - (st.tone_phase + 0.5).floor())
                        }
                        WaveformKind::Noise => fastrand_f64() * 2.0 - 1.0,
                    };

                    let val = (sample * volume as f64) as f32;

                    for c in 0..channels {
                        let start = i * stride + c * bps;
                        write_sample(&mut slice[start..start + bps], val, sample_fmt);
                    }

                    st.tone_phase += phase_inc;
                    if st.tone_phase >= 1.0 {
                        st.tone_phase -= 1.0;
                    }
                }

                st.tone_samples_produced += n_frames as u64;
                if let Some(max) = st.tone_duration_samples
                    && st.tone_samples_produced >= max
                {
                    st.active_job = None;
                }
            }
            Some(ActiveJob::Playback { .. }) => {
                let buf_len = st.playback_buffer.len();
                if buf_len == 0 {
                    slice.fill(0);
                } else {
                    let vol = st.playback_volume;
                    'frames: for i in 0..n_frames {
                        for c in 0..channels {
                            if st.playback_pos >= buf_len {
                                match st.playback_loop_count {
                                    Some(0) => {
                                        st.playback_pos = 0;
                                    }
                                    Some(n) => {
                                        st.playback_loops_done += 1;
                                        if st.playback_loops_done >= n {
                                            fill_silence(
                                                slice, i, n_frames, channels, stride, bps,
                                                sample_fmt,
                                            );
                                            st.active_job = None;
                                            break 'frames;
                                        }
                                        st.playback_pos = 0;
                                    }
                                    None => {
                                        fill_silence(
                                            slice, i, n_frames, channels, stride, bps,
                                            sample_fmt,
                                        );
                                        st.active_job = None;
                                        break 'frames;
                                    }
                                }
                            }
                            let val = st.playback_buffer[st.playback_pos] * vol;
                            let start = i * stride + c * bps;
                            write_sample(&mut slice[start..start + bps], val, sample_fmt);
                            st.playback_pos += 1;
                        }
                    }
                }
            }
            Some(ActiveJob::Streaming {
                direction: StreamDirection::In,
            })
            | Some(ActiveJob::Loopback) => {
                // Read from the stream-in / loopback ring buffer
                if let Some(ref rb) = st.stream_in_buffer {
                    let n_samples = n_frames * channels;
                    let mut temp = vec![0.0f32; n_samples];
                    let read = rb.read(&mut temp);
                    // Underrun pads with silence rather than replaying stale
                    // ring contents.
                    temp[read..].fill(0.0);
                    for (i, &val) in temp.iter().enumerate() {
                        let start = i * bps;
                        if start + bps <= slice.len() {
                            write_sample(&mut slice[start..start + bps], val, sample_fmt);
                        }
                    }
                } else {
                    slice.fill(0);
                }
            }
            _ => {
                slice.fill(0);
            }
        }

        n_frames
    } else {
        0
    };

    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = stride as _;
    *chunk.size_mut() = (stride * n_frames) as _;
}

#[inline]
fn fill_silence(
    slice: &mut [u8],
    from_frame: usize,
    n_frames: usize,
    channels: usize,
    stride: usize,
    bps: usize,
    fmt: SampleFormat,
) {
    for j in from_frame..n_frames {
        for c in 0..channels {
            let s = j * stride + c * bps;
            write_sample(&mut slice[s..s + bps], 0.0, fmt);
        }
    }
}

fn sink_process(stream: &StreamRef, state: &Rc<RefCell<SinkState>>) {
    let mut buffer = match stream.dequeue_buffer() {
        Some(b) => b,
        None => return,
    };

    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return;
    }

    let data = &mut datas[0];
    // Read chunk size before taking the mutable slice reference
    let chunk_size = data.chunk().size() as usize;

    let sample_fmt = match state.try_borrow() {
        Ok(st) => st.format.sample_format,
        Err(_) => return,
    };
    let bps = sample_fmt.bytes_per_sample();

    if let Some(slice) = data.data() {
        // Use try_borrow_mut to avoid panicking if a command handler
        // currently holds the borrow (timer + process callback can overlap).
        let mut st = match state.try_borrow_mut() {
            Ok(s) => s,
            Err(_) => return, // Skip this buffer; command in progress
        };
        if st.active_job.is_some() {
            // Use the actual data size from the chunk, not the full buffer capacity
            let n_samples = if chunk_size > 0 {
                chunk_size / bps
            } else {
                slice.len() / bps
            };
            let mut samples = Vec::with_capacity(n_samples);
            for i in 0..n_samples {
                let start = i * bps;
                if start + bps > slice.len() {
                    break;
                }
                samples.push(read_sample(&slice[start..start + bps], sample_fmt));
            }

            // Write to stream-out ring buffer if streaming
            if let Some(ref rb) = st.stream_out_buffer {
                rb.write(&samples);
            }

            // Feed the source when looping back
            if let Some(ref rb) = st.loopback_buffer {
                rb.write(&samples);
            }

            // Hand recorded audio to the writer thread
            if st.recorder.is_some() && !st.record_complete {
                let remaining = match st.record_limit_samples {
                    Some(limit) => limit.saturating_sub(st.record_samples) as usize,
                    None => samples.len(),
                };
                let take = remaining.min(samples.len());
                if take > 0 {
                    if let Some(ref rec) = st.recorder {
                        rec.push(&samples[..take]);
                    }
                    st.record_samples += take as u64;
                }
                if let Some(limit) = st.record_limit_samples
                    && st.record_samples >= limit
                {
                    // Finalized by DevicePair::poll off the RT thread.
                    st.record_complete = true;
                }
            }
        }
    }
}

fn fastrand_f64() -> f64 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = const { Cell::new(0x12345678_9abcdef0) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x as f64) / (u64::MAX as f64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_roundtrip_preserves_value() {
        for fmt in [SampleFormat::F32LE, SampleFormat::S16LE, SampleFormat::S32LE] {
            let mut buf = vec![0u8; fmt.bytes_per_sample()];
            for v in [0.0f32, 0.5, -0.5, 1.0, -1.0] {
                write_sample(&mut buf, v, fmt);
                let got = read_sample(&buf, fmt);
                assert!(
                    (got - v).abs() < 0.001,
                    "{fmt}: wrote {v}, read back {got}"
                );
            }
        }
    }

    #[test]
    fn stride_follows_sample_format() {
        assert_eq!(SampleFormat::S16LE.bytes_per_sample(), 2);
        assert_eq!(SampleFormat::S32LE.bytes_per_sample(), 4);
        assert_eq!(SampleFormat::F32LE.bytes_per_sample(), 4);
    }
}
