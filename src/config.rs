use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub sound: SoundConfig,
    #[serde(default)]
    pub general: GeneralConfig,
}

/// Bounds for externally-supplied (hand-edited) [`SoundConfig`] values, applied in
/// [`Config::load`] so tone generation can't overflow or emit a garbage waveform.
const MAX_TONE_DURATION_MS: u32 = 5_000;
const MIN_TONE_FREQ_HZ: u32 = 20;
const MAX_TONE_FREQ_HZ: u32 = 20_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundConfig {
    pub enabled: bool,
    pub volume: f32,
    pub freq_high_hz: u32,
    pub freq_low_hz: u32,
    pub duration_ms: u32,
}

impl SoundConfig {
    /// Clamp hand-edited values into safe ranges: a non-finite/out-of-range volume
    /// yields a NaN/garbage tone, and an absurd `duration_ms` overflows the sample
    /// count in `generate_tone`.
    fn sanitize(&mut self) {
        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            Self::default().volume
        };
        self.duration_ms = self.duration_ms.min(MAX_TONE_DURATION_MS);
        self.freq_high_hz = self.freq_high_hz.clamp(MIN_TONE_FREQ_HZ, MAX_TONE_FREQ_HZ);
        self.freq_low_hz = self.freq_low_hz.clamp(MIN_TONE_FREQ_HZ, MAX_TONE_FREQ_HZ);
    }
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: 0.5,
            freq_high_hz: 1000,
            freq_low_hz: 700,
            duration_ms: 150,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GeneralConfig {
    #[serde(default)]
    pub auto_start: bool,
    /// Microphone dB boost (0, 5, 10)
    #[serde(default)]
    pub mic_boost_db: u8,
}

impl Config {
    /// Return the config file path.
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("corsair-void")
            .join("config.toml")
    }

    /// Load config from disk. Returns defaults if the file doesn't exist.
    pub fn load() -> Self {
        let path = Self::path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => match toml::from_str::<Self>(&contents) {
                    Ok(mut config) => {
                        config.sanitize();
                        info!("Loaded config from {}", path.display());
                        return config;
                    }
                    Err(e) => warn!("Failed to parse config: {}. Using defaults.", e),
                },
                Err(e) => warn!("Failed to read config: {}. Using defaults.", e),
            }
        }
        Self::default()
    }

    /// Normalize externally-supplied values into valid ranges. Defaults are already
    /// valid, so this only needs to run on configs parsed from disk.
    fn sanitize(&mut self) {
        self.sound.sanitize();
        self.general.mic_boost_db = crate::audio::normalize_boost_db(self.general.mic_boost_db);
    }

    /// Save config to disk, creating directories if needed.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)?;
        info!("Saved config to {}", path.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_clamps_out_of_range_values() {
        let mut config = Config::default();
        config.sound.volume = f32::NAN;
        config.sound.duration_ms = 10_000_000;
        config.sound.freq_high_hz = 5_000_000;
        config.sound.freq_low_hz = 0;
        config.general.mic_boost_db = 200;

        config.sanitize();

        assert_eq!(config.sound.volume, SoundConfig::default().volume);
        assert!(config.sound.duration_ms <= MAX_TONE_DURATION_MS);
        assert!(config.sound.freq_high_hz <= MAX_TONE_FREQ_HZ);
        assert!(config.sound.freq_low_hz >= MIN_TONE_FREQ_HZ);
        assert_eq!(config.general.mic_boost_db, 10);
    }
}
