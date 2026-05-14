use log::warn;
use rodio::{OutputStream, Sink, buffer::SamplesBuffer};
use std::thread;

use crate::config::SoundConfig;

const SAMPLE_RATE: u32 = 44100;

pub struct SoundPlayer {
    config: SoundConfig,
}

impl SoundPlayer {
    pub fn new(config: &SoundConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }

    /// Play low battery warning: low → high → low (700 Hz → 1000 Hz → 700 Hz).
    /// Runs in a detached thread so the caller (tray main thread) does not block.
    /// Recreates the OutputStream per call so a changed default device is picked up.
    pub fn play_low_battery(&self) {
        if !self.config.enabled {
            return;
        }
        let config = self.config.clone();
        thread::spawn(move || {
            let (_stream, handle) = match OutputStream::try_default() {
                Ok(s) => s,
                Err(e) => {
                    warn!("Failed to open default audio output: {}", e);
                    return;
                }
            };
            let sink = match Sink::try_new(&handle) {
                Ok(s) => s,
                Err(e) => {
                    warn!("Failed to create audio sink: {}", e);
                    return;
                }
            };
            sink.set_volume(config.volume);

            for &freq in &[config.freq_low_hz, config.freq_high_hz, config.freq_low_hz] {
                let samples = generate_tone(freq as f32, config.duration_ms, SAMPLE_RATE);
                let buffer = SamplesBuffer::new(1, SAMPLE_RATE, samples);
                sink.append(buffer);
            }

            sink.sleep_until_end();
        });
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
