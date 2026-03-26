use log::warn;
use rodio::{OutputStream, OutputStreamHandle, Sink, buffer::SamplesBuffer};

use crate::config::SoundConfig;

const SAMPLE_RATE: u32 = 44100;

pub struct SoundPlayer {
    _stream: OutputStream,
    stream_handle: OutputStreamHandle,
    config: SoundConfig,
}

impl SoundPlayer {
    pub fn new(config: &SoundConfig) -> Option<Self> {
        match OutputStream::try_default() {
            Ok((stream, handle)) => Some(Self {
                _stream: stream,
                stream_handle: handle,
                config: config.clone(),
            }),
            Err(e) => {
                warn!("Failed to initialize audio output: {}", e);
                None
            }
        }
    }

    /// Play low battery warning: low → high → low (700 Hz → 1000 Hz → 700 Hz)
    pub fn play_low_battery(&self) {
        if !self.config.enabled {
            return;
        }
        let sink = match Sink::try_new(&self.stream_handle) {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to create audio sink: {}", e);
                return;
            }
        };

        sink.set_volume(self.config.volume);

        for &freq in &[
            self.config.freq_low_hz,
            self.config.freq_high_hz,
            self.config.freq_low_hz,
        ] {
            let samples = generate_tone(freq as f32, self.config.duration_ms, SAMPLE_RATE);
            let buffer = SamplesBuffer::new(1, SAMPLE_RATE, samples);
            sink.append(buffer);
        }

        sink.sleep_until_end();
    }
}

fn generate_tone(freq: f32, duration_ms: u32, sample_rate: u32) -> Vec<f32> {
    let samples = (sample_rate * duration_ms / 1000) as usize;
    (0..samples)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            (2.0 * std::f32::consts::PI * freq * t).sin() * 0.5
        })
        .collect()
}
