use log::warn;
use rodio::{buffer::SamplesBuffer, OutputStream, Sink};
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
    // Compute in u64: `sample_rate * duration_ms` overflows u32 at ~97 s, which
    // panics in debug and wraps in release. Config values are already clamped on
    // load, but the wider arithmetic keeps this correct regardless of the caller.
    let samples = (sample_rate as u64 * duration_ms as u64 / 1000) as usize;
    (0..samples)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            (2.0 * std::f32::consts::PI * freq * t).sin() * 0.5
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_tone_large_duration_does_not_overflow() {
        // 100_000 ms overflows the old `sample_rate * duration_ms` u32 math
        // (panics in debug, wraps in release); u64 keeps the count correct.
        let samples = generate_tone(1000.0, 100_000, SAMPLE_RATE);
        assert_eq!(samples.len(), SAMPLE_RATE as usize * 100);
    }
}
