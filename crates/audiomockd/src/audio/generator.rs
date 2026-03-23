use std::f64::consts::PI;

use audiomock_proto::audio::WaveformKind;

/// Generate a buffer of f32 tone samples (interleaved).
pub fn generate_tone(
    waveform: WaveformKind,
    frequency: f64,
    volume: f32,
    sample_rate: u32,
    channels: u16,
    duration_secs: f64,
) -> Vec<f32> {
    let total_frames = (sample_rate as f64 * duration_secs) as usize;
    let total_samples = total_frames * channels as usize;
    let mut output = Vec::with_capacity(total_samples);

    let phase_inc = frequency / sample_rate as f64;
    let mut phase = 0.0;

    for _ in 0..total_frames {
        let sample = match waveform {
            WaveformKind::Sine => (phase * 2.0 * PI).sin(),
            WaveformKind::Square => {
                if (phase * 2.0 * PI).sin() >= 0.0 {
                    1.0
                } else {
                    -1.0
                }
            }
            WaveformKind::Sawtooth => 2.0 * (phase - (phase + 0.5).floor()),
            WaveformKind::Noise => simple_rand() * 2.0 - 1.0,
        };

        let val = (sample * volume as f64) as f32;
        for _ in 0..channels {
            output.push(val);
        }

        phase += phase_inc;
        if phase >= 1.0 {
            phase -= 1.0;
        }
    }

    output
}

fn simple_rand() -> f64 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = const { Cell::new(0xdeadbeef_cafebabe) };
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
