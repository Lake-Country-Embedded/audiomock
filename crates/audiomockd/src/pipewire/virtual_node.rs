use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use audiomock_proto::audio::{AudioFormat, WaveformKind};
use audiomock_proto::device::{ActiveJob, DeviceInfo, StreamDirection};
use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;
use pw::stream::{Stream, StreamFlags, StreamRef};

use super::format;
use crate::audio::ring_buffer::RingBuffer;

const CHAN_SIZE: usize = std::mem::size_of::<f32>();

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
    /// Ring buffer for stream-in: IPC writes, process callback reads.
    pub stream_in_buffer: Option<Arc<RingBuffer>>,
}

/// Shared state for the virtual sink.
pub struct SinkState {
    pub format: AudioFormat,
    pub active_job: Option<ActiveJob>,
    pub record_buffer: Vec<f32>,
    pub record_file_path: Option<String>,
    /// Ring buffer for stream-out: process callback writes, IPC reads.
    pub stream_out_buffer: Option<Arc<RingBuffer>>,
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
            record_buffer: Vec::new(),
            record_file_path: None,
            stream_out_buffer: None,
        }));

        // --- Source stream (virtual mic, produces audio) ---
        // Use Stream/Output/Audio so PipeWire exposes output ports that
        // can be linked to QEMU's input (mic). Audio/Source/Virtual creates
        // a device node whose ports aren't directly linkable.
        let source_props = pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_CLASS => "Stream/Output/Audio",
            *pw::keys::NODE_NAME => format!("audiomock-source-{name}"),
            "node.always-process" => "true",
            "node.want-driver" => "true",
            *pw::keys::NODE_DESCRIPTION => source_desc,
        };

        let source_stream =
            Stream::new(core, &format!("audiomock-source-{name}"), source_props)?;

        let channels = audio_format.channels;
        let src_state_ref = source_state.clone();

        let source_listener = source_stream
            .add_local_listener_with_user_data(())
            .process(move |stream, _| {
                source_process(stream, &src_state_ref, channels);
            })
            .register()?;

        let format_bytes = format::build_audio_format_bytes(audio_format);
        let mut params = [Pod::from_bytes(&format_bytes).unwrap()];

        source_stream.connect(
            spa::utils::Direction::Output,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
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
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
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
        {
            let mut state = self.sink_state.borrow_mut();
            state.active_job = None;
            let buf_len = state.record_buffer.len();
            if let Some(ref path) = state.record_file_path {
                tracing::info!(
                    "Flushing recording: {} samples to '{path}'",
                    buf_len
                );
                if buf_len > 0 {
                    flush_recording(&state.record_buffer, path, &state.format);
                } else {
                    tracing::warn!("Recording buffer is empty — sink received no audio");
                }
            }
            state.record_buffer.clear();
            state.record_file_path = None;
            state.stream_out_buffer = None;
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

    pub fn start_recording(&mut self, file_path: &str) -> Result<()> {
        let mut state = self.sink_state.borrow_mut();
        let old_len = state.record_buffer.len();
        state.record_buffer.clear();
        state.record_file_path = Some(file_path.to_string());
        state.active_job = Some(ActiveJob::Recording {
            file_path: file_path.to_string(),
        });
        tracing::info!(
            "Starting recording to '{file_path}' on '{}' (cleared {} old samples)",
            self.name, old_len
        );
        Ok(())
    }
}

fn source_process(stream: &StreamRef, state: &Rc<RefCell<SourceState>>, channels: u16) {
    let mut buffer = match stream.dequeue_buffer() {
        Some(b) => b,
        None => return,
    };

    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return;
    }

    let data = &mut datas[0];
    let stride = CHAN_SIZE * channels as usize;

    let n_frames = if let Some(slice) = data.data() {
        let n_frames = slice.len() / stride;
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
                    let val_bytes = f32::to_le_bytes(val);

                    for c in 0..channels as usize {
                        let start = i * stride + c * CHAN_SIZE;
                        let end = start + CHAN_SIZE;
                        slice[start..end].copy_from_slice(&val_bytes);
                    }

                    st.tone_phase += phase_inc;
                    if st.tone_phase >= 1.0 {
                        st.tone_phase -= 1.0;
                    }
                }

                st.tone_samples_produced += n_frames as u64;
                if let Some(max) = st.tone_duration_samples {
                    if st.tone_samples_produced >= max {
                        st.active_job = None;
                    }
                }
            }
            Some(ActiveJob::Playback { .. }) => {
                let buf_len = st.playback_buffer.len();
                if buf_len == 0 {
                    slice.fill(0);
                } else {
                    let vol = st.playback_volume;
                    'frames: for i in 0..n_frames {
                        for c in 0..channels as usize {
                            if st.playback_pos >= buf_len {
                                match st.playback_loop_count {
                                    Some(0) => {
                                        st.playback_pos = 0;
                                    }
                                    Some(n) => {
                                        st.playback_loops_done += 1;
                                        if st.playback_loops_done >= n {
                                            // Fill rest with silence
                                            for j in i..n_frames {
                                                for c2 in 0..channels as usize {
                                                    let s = j * stride + c2 * CHAN_SIZE;
                                                    let e = s + CHAN_SIZE;
                                                    slice[s..e].copy_from_slice(
                                                        &f32::to_le_bytes(0.0),
                                                    );
                                                }
                                            }
                                            st.active_job = None;
                                            break 'frames;
                                        }
                                        st.playback_pos = 0;
                                    }
                                    None => {
                                        for j in i..n_frames {
                                            for c2 in 0..channels as usize {
                                                let s = j * stride + c2 * CHAN_SIZE;
                                                let e = s + CHAN_SIZE;
                                                slice[s..e]
                                                    .copy_from_slice(&f32::to_le_bytes(0.0));
                                            }
                                        }
                                        st.active_job = None;
                                        break 'frames;
                                    }
                                }
                            }
                            let val = st.playback_buffer[st.playback_pos] * vol;
                            let start = i * stride + c * CHAN_SIZE;
                            let end = start + CHAN_SIZE;
                            slice[start..end].copy_from_slice(&f32::to_le_bytes(val));
                            st.playback_pos += 1;
                        }
                    }
                }
            }
            Some(ActiveJob::Streaming { direction: StreamDirection::In }) => {
                // Read from the stream-in ring buffer
                if let Some(ref rb) = st.stream_in_buffer {
                    let n_floats = n_frames * channels as usize;
                    let mut temp = vec![0.0f32; n_floats];
                    let read = rb.read(&mut temp);
                    for i in 0..n_floats {
                        let val = if i < read { temp[i] } else { 0.0 };
                        let start = i * CHAN_SIZE;
                        let end = start + CHAN_SIZE;
                        if end <= slice.len() {
                            slice[start..end].copy_from_slice(&f32::to_le_bytes(val));
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
    if let Some(slice) = data.data() {
        // Use try_borrow_mut to avoid panicking if a command handler
        // currently holds the borrow (timer + process callback can overlap).
        let mut st = match state.try_borrow_mut() {
            Ok(s) => s,
            Err(_) => return, // Skip this buffer; command in progress
        };
        if st.active_job.is_some() {
            // Use the actual data size from the chunk, not the full buffer capacity
            let n_floats = if chunk_size > 0 {
                chunk_size / CHAN_SIZE
            } else {
                slice.len() / CHAN_SIZE
            };
            let mut samples = Vec::with_capacity(n_floats);
            for i in 0..n_floats {
                let start = i * CHAN_SIZE;
                let end = start + CHAN_SIZE;
                let val =
                    f32::from_le_bytes(slice[start..end].try_into().unwrap_or([0; 4]));
                samples.push(val);
            }

            // Write to stream-out ring buffer if streaming
            if let Some(ref rb) = st.stream_out_buffer {
                rb.write(&samples);
            }

            // Also accumulate in record buffer if recording
            if st.record_file_path.is_some() {
                st.record_buffer.extend_from_slice(&samples);
            }
        }
    }
}

fn flush_recording(buffer: &[f32], path: &str, format: &AudioFormat) {
    if buffer.is_empty() {
        return;
    }
    if let Err(e) = crate::audio::encoder::write_wav(buffer, path, format) {
        tracing::error!("Failed to write recording to {path}: {e}");
    } else {
        tracing::info!("Wrote recording to {path}");
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
